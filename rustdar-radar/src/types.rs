use crate::site_position::{CATALOGUE_DISAGREEMENT_LIMIT_KM, SitePosition, SitePositionSource};
use crate::sites::RadarSite;
use crate::sites::get_radar_site;
use chrono::NaiveDateTime;
use nexrad_model::data::Radial;
use nexrad_model::data::Scan;
use rustdar_units::{Quantity, UserPreferences};
use std::collections::HashMap;

/// The wasm32 side length, named **outside** the [`IMAGE_SIZE`] cascade so that
/// it is reachable from a host build's tests.
///
/// A `cfg`-selected literal can only be checked by the target that compiles it,
/// and this workspace runs `cargo test` on exactly one of the two arms. Spelt as
/// a literal inside the cascade this value was free: an audit changed it to 4096
/// on a pristine tree and watched the whole workspace pass 1508/0 with
/// `cargo check --target wasm32-unknown-unknown` exiting 0 — while 4096 is twice
/// the largest 2D texture WebGL2 guarantees, so every browser render would have
/// failed. Both arms now have names, so both arms can be asserted.
///
/// It is [`WEBGL2_MAX_TEXTURE_DIMENSION_2D`] itself, which is the most a browser
/// can be asked for. It used to be half that, on the reasoning that a 2048²
/// frame "sits exactly on the limit with nothing spare for the overlay textures
/// beside it" — and that reasoning was wrong about what the limit is.
/// `max_texture_dimension_2d` bounds each texture's each axis, not the sum of a
/// frame's textures, and the overlays are sized from the viewport and clamped
/// against the same limit independently (`rustdar_egui::overlay_cache::
/// plan_overlay_texture`, handed the adapter's real figure). Nothing was ever
/// competing for the 2048.
pub const WASM_IMAGE_SIZE: usize = 2048;

/// The native side length. See [`WASM_IMAGE_SIZE`].
pub const NATIVE_IMAGE_SIZE: usize = 2048;

/// The largest 2D texture WebGL2 — and so a browser — is *guaranteed* to accept
/// per axis.
///
/// Written here rather than derived from wgpu because this crate has no wgpu
/// dependency and must not grow one: it is the rasterizer, and it hands finished
/// RGBA buffers to a caller that owns the GPU. `rustdar-frontend`'s
/// `the_web_image_fits_the_texture_size_webgl2_guarantees` checks this figure
/// against `wgpu::Limits::downlevel_webgl2_defaults()` from the crate that does
/// have wgpu, so the number cannot drift away from wgpu's own.
pub const WEBGL2_MAX_TEXTURE_DIMENSION_2D: usize = 2048;

/// Side length, in pixels, of the square radar image a render produces at
/// [`BASE_EXTENT_KM`] and at every extent inside it.
/// An RGBA texture is `IMAGE_SIZE² × 4` bytes; a static pane render keeps an
/// `f32` value grid alongside it, doubling that.
///
/// Both arms are 2048 today, which is a coincidence of the two ceilings rather
/// than a simplification waiting to happen: native's is what a 230 km frame
/// costs at 4.45 px/km, and the browser's is the largest texture WebGL2
/// guarantees ([`WEBGL2_MAX_TEXTURE_DIMENSION_2D`]). The two move for different
/// reasons, so they keep separate names.
///
/// It is no longer the only side a render can have. A sweep reaching past the
/// floor is projected onto more ground, and [`raster_side_px`] is what decides
/// how many pixels that ground gets — up to a ceiling the *caller* owns, because
/// how large a texture this machine can take is not a fact this crate has.
///
/// The two arms select between [`WASM_IMAGE_SIZE`] and [`NATIVE_IMAGE_SIZE`]
/// rather than repeating their literals, so the *selection* is the only thing
/// here a host build cannot check.
#[cfg(target_arch = "wasm32")]
pub const IMAGE_SIZE: usize = WASM_IMAGE_SIZE;
#[cfg(not(target_arch = "wasm32"))]
pub const IMAGE_SIZE: usize = NATIVE_IMAGE_SIZE;

/// The half-width [`IMAGE_SIZE`] is calibrated at, km: the extent at which the
/// base texture is 4.4522 px/km.
///
/// 230 km is the WSR-88D's nominal unambiguous range and the extent this
/// rasterizer was fixed at for its whole life. **It is no longer a floor under
/// the extent** — [`plan_view_extent_km`] projects a plan view at the range its
/// own data reaches, and nothing here raises a short sweep to meet this number.
/// What is left of it is the *resolution* reference in [`raster_side_px`]: a
/// render at this extent or inside it fits in the base texture, and only a
/// render past it needs the caller's larger ceiling.
///
/// # What the floor was doing, measured
///
/// It never touched a WSR-88D's lowest cuts. A split cut's Doppler half carries
/// 1192 gates of 0.25 km from a first gate 2.125 km out — 300.125 km of slant
/// range, ±300.11 on the ground at half a degree — and the RDA states the intent
/// in the volume coverage pattern itself, where those elevation cuts carry
/// `super_resolution_doppler_to_300km`. Identical on eight sites across three
/// patterns (KCBW, KESX, KICT, KMPX, KUDX, KCRP, KFTG, KDMX; VCP 35, 212, 215):
/// the 1192-gate geometry is every velocity tilt from the lowest up to about
/// 3°, and the surveillance halves beside them are 1832 gates — 460.125 km
/// slant, ±460.11 on the ground.
///
/// What it did touch was **every TDWR Doppler moment**, the volume's upper half,
/// and the products that are not a tilt. A TDWR's velocity and spectrum width
/// are 592 gates of 0.15 km from 0.0 km out — 88.800 km slant, ±88.80 on the
/// ground — on all four sites measured (TOKC, TDAL, TPIT, TATL, 2026-08-11 00Z),
/// beside a 1390-gate reflectivity reaching 417 km. Raised to 230 km, that
/// Doppler disc was 11.7% of its own raster and every one of its 0.15 km gates
/// was 0.668 px across: **sub-pixel, so gates fought each other for pixels**.
/// At its own extent the disc is 78.5% of the raster and a gate is 1.73 px.
/// The first WSR-88D cut ending short of 230 km is the one near 5° (208.1 km at
/// KCBW, 197.1 at KESX, 210.1 at KCRP), and every tilt above it is shorter
/// again; every derived 1° × 1 km grid and every Level III product this display
/// fetches was inside the floor too, and so was being drawn on a frame wider
/// than its data.
///
/// `a_tdwr_doppler_sweep_is_projected_at_its_own_reach_not_the_base_extent` is
/// where the TDWR case is pinned, and
/// `a_short_sweep_gets_the_base_texture_over_its_own_ground` is where this
/// constant's surviving role is.
pub const BASE_EXTENT_KM: f64 = 230.0;

/// The half-width to project a plan view at when the scan does not say how far
/// its data reached, km.
///
/// **A fallback and only a fallback.** [`plan_view_extent_km`] reaches it on a
/// `NaN` reach and on a non-positive one, which between them are the two ways a
/// render can be asked for a picture of nothing: a product no radial of the
/// sweep carries (`crate::render`'s `compute_max_range` answers 0.0), and a
/// derived grid that came back with no range bins. Both paint an empty raster,
/// so what this number decides is the size of an empty frame and never where an
/// echo goes.
///
/// It is 230 km because that is the extent this display has always drawn an
/// otherwise-unexplained frame at, and it is spelt separately from
/// [`BASE_EXTENT_KM`] rather than reusing it because the two answer different
/// questions and would move for different reasons — the same argument
/// [`WASM_IMAGE_SIZE`] and [`NATIVE_IMAGE_SIZE`] are kept apart on. One is the
/// extent a texture size was calibrated at; this one is what to draw when there
/// is nothing to draw.
///
/// `an_unstated_reach_is_the_only_way_to_reach_the_fallback_extent` is the
/// guard that it stays unreachable from any sweep that states a reach.
pub const FALLBACK_EXTENT_KM: f64 = 230.0;

/// The furthest half-width a plan view will project at, km.
///
/// Not a range any radar reaches: the longest real reach in this display is a
/// WSR-88D surveillance cut at 2.125 + 1832 × 0.25 = 460.125 km, and a TDWR's
/// long-range reflectivity is 1390 × 0.3 km = 417 km. It is a ceiling on
/// *arithmetic*, because the extent is now derived from a gate count that
/// arrives over the wire: a mis-framed radial claiming sixty thousand gates
/// would otherwise zoom the whole display out to a continent. 470 km clears
/// the widest honest sweep by 9.9 km and turns every impossible one into a
/// render that is merely too coarse.
pub const MAX_EXTENT_KM: f64 = 470.0;

/// The half-width to project a plan view at, km: **how far this data reaches**,
/// and nothing else.
///
/// The reach comes from the sweep itself ([`crate::render`]'s
/// `compute_max_range`, the per-sweep counterpart of
/// [`crate::voxel::volume_reach_km`]), so this is the one place the raster's
/// geometry is decided and it is now a measurement rather than a decision.
/// [`MAX_EXTENT_KM`] is the only bound left, and it bounds *arithmetic*, not
/// data: a mis-framed radial claiming sixty thousand gates is refused.
///
/// # There is no floor, and that is the change
///
/// This used to `clamp` the reach up to [`BASE_EXTENT_KM`], so a sweep that
/// stopped inside 230 km was projected onto a 230 km frame anyway. Every
/// consumer of the returned figure treats it as the edge of the picture — the
/// texture's corners, the hover's divisor, the plan view's range ring — so a
/// TDWR velocity sweep reaching 88.8 km was drawn with a ring at 230 km around
/// it, which states coverage the radar did not have. The extent is the data's
/// now, so the ring is the data's, and none of those consumers had to learn
/// anything new.
///
/// Nor is there a *lower* clamp to replace the floor with. A floor at any value
/// is the same claim in a smaller font, and the reach is already a maximum over
/// the sweep's radials, so no single short radial can drag it down.
///
/// # The one case that is not the data's
///
/// A `NaN` reach and a non-positive reach both answer [`FALLBACK_EXTENT_KM`],
/// spelt as an early return so the fallback is legible as one. Those are the
/// two shapes of "the scan does not say": `compute_max_range` returns 0.0 when
/// no radial of the sweep carries the product's moment, a derived grid with no
/// range bins arrives as 0.0, and a `NaN` cannot come off the wire today —
/// gate counts and intervals are integers — but `clamp` propagates one, and a
/// `NaN` extent reaches [`ImageBounds`] and makes every pixel of the render
/// unplaceable with no error anywhere. All three paint nothing, so the fallback
/// sizes an empty frame and never places an echo.
///
/// An infinite reach needs no special case: it is the cap's case taken to its
/// limit, and `min` answers it correctly.
///
/// `a_tdwr_doppler_sweep_is_projected_at_its_own_reach_not_the_base_extent`
/// pins the reported defect and
/// `an_unstated_reach_is_the_only_way_to_reach_the_fallback_extent` pins the
/// fallback's reachability.
pub fn plan_view_extent_km(data_reach_km: f64) -> f64 {
    // `is_nan` spelled out rather than folded into the comparison: every
    // ordering against a `NaN` is false, so `<= 0.0` alone would let one
    // through to arithmetic that propagates it.
    if data_reach_km.is_nan() || data_reach_km <= 0.0 {
        return FALLBACK_EXTENT_KM;
    }
    data_reach_km.min(MAX_EXTENT_KM)
}

/// How many pixels across to paint a plan view of `extent_km`, given the
/// largest side this caller can accept.
///
/// [`plan_view_extent_km`] decides how much *ground* a raster covers;
/// this decides how finely that ground is sampled. The split is on purpose and
/// it is the answer to two different questions: **how much ground a picture
/// shows is a fact about the data, and how finely it is sampled is a fact about
/// the device.** A picture whose ground moved with the machine looking at it
/// would put the same volume's echo in two places — [`ImageBounds`] takes the
/// extent as an argument precisely so a raster and its corners cannot disagree
/// — and a loop frame, which renders leaner on purpose, would crop the pane it
/// is playing in rather than merely softening it.
///
/// At [`BASE_EXTENT_KM`] the answer is [`IMAGE_SIZE`], so a 230 km sweep is the
/// same 4.4522 px/km it has always been, and **inside** it the same texture is
/// spent on less ground: a TDWR Doppler sweep reaching 88.8 km is 11.5319
/// px/km, 2.59× finer than it was while every short sweep was drawn on a 230 km
/// frame. Past it, whether the extra ground is free depends entirely on the
/// ceiling on offer.
///
/// # What each ceiling buys, measured
///
/// A real KDMX 0.53° cut (2022-03-05 23:23), rendered through this crate:
///
/// | sweep                          | extent    | 4096 ceiling | 2048 ceiling |
/// |--------------------------------|----------:|-------------:|-------------:|
/// | velocity, 1192 gates of 0.25 km| ±300.11 km|  6.8241 px/km|  3.4121 px/km|
/// | reflectivity, 1832 gates       | ±460.11 km|  4.4512 px/km|  2.2256 px/km|
///
/// Against [`BASE_EXTENT_KM`]'s 4.4522 px/km, a 4096 ceiling is where the second number
/// pays for itself: the Doppler cut comes out **finer** than that and the
/// surveillance cut lands 0.022% under it, which is the same picture over 1.7×
/// and 4.0× the ground.
///
/// # What a ceiling at the base size does instead, and why that is accepted
///
/// A ceiling of [`IMAGE_SIZE`] cannot buy pixels, so the wider frame is paid
/// for in scale: that Doppler cut is 3.4121 px/km, **23.4% coarser** than the
/// reference, and a 0.25 km gate goes from 1.11 pixels of its own to 0.85 of one.
/// Two callers are in that case — a browser, where 2048 is the largest texture
/// WebGL2 guarantees, and a GLES 3.0 handheld reporting the spec floor.
///
/// It is a real cost and it is taken deliberately, because the alternative is
/// worse than it looks. Holding those devices at the reference scale means
/// holding them at the reference *extent*, and that trades a picture that is
/// uniformly softer for one that is missing its outer third — on a Doppler cut,
/// everything from 230 km to 300 km, which over 192 sweeps of the eight sites
/// above is 448 690 gates carrying a velocity, 3.4% of all the velocity those
/// sweeps hold. It would also make a pane's ground coverage depend on which
/// machine opened the file, and make a loop frame narrower than the still frame
/// it replaces.
///
/// What is guaranteed instead is that the base-size arm stays inside the
/// display's own resolution line: the widest sweep a radar flies is 2.2256
/// px/km there and even [`MAX_EXTENT_KM`]'s arithmetic guard is 2.1787, both
/// past the two-pixels-per-kilometre mark below which a 250 m gate stops
/// landing in a pixel at all. `a_base_size_ceiling_pays_for_the_extra_ground_
/// in_scale` is where that is pinned, on a 1192-gate Doppler cut.
///
/// # Why the ceiling is an argument
///
/// The largest texture this machine will accept is not a fact this crate has.
/// It is a `wgpu` device limit, read by `rustdar-frontend`, and it is a
/// *runtime* answer: Vulkan guarantees 4096, iOS Metal offers 8192, and the
/// GLES 3.0 floor is 2048, so an Android handheld can be the one device that
/// cannot take the long-range raster. The build-script `cfg` that names the
/// device class does not cross a crate boundary either (see
/// [`crate::voxel`]'s module doc for that trap), so there is no honest way to
/// decide this here. The caller passes what it can take, and gets back what it
/// gets.
///
/// # Why it bounds the short range too, and not only the long
///
/// A ceiling *under* [`IMAGE_SIZE`] is a real request, not a mistake to clamp
/// away: the browser renders its loop frames at 1024 on purpose — the eight it
/// textures at once would be 128 MiB at 2048², against a 48 MiB per-pane loop
/// budget — while its static renders take the full 2048. So `min` rather than
/// "base unless the extent is long": a caller that says 1024 means 1024, and a
/// caller that says 4096 gets the base size until there is ground to spend the
/// rest on.
///
/// Passing exactly [`IMAGE_SIZE`] therefore fixes the raster's side for every
/// extent, which is the device gate's whole mechanism: a machine that cannot
/// take the long-range texture asks for the base one and gets a correct
/// picture rather than a texture creation that fails and leaves a blank pane.
/// It is the *side* that stops moving, not the picture — the extent is the
/// data's either way, so what that caller receives is the section above's
/// coarser frame over the same ground, not a different frame.
pub fn raster_side_px(extent_km: f64, side_ceiling_px: usize, sample_km: f64) -> usize {
    if extent_km > BASE_EXTENT_KM {
        side_ceiling_px.min(data_limited_side_px(extent_km, sample_km))
    } else {
        IMAGE_SIZE.min(side_ceiling_px)
    }
}

/// Texels per sample the raster is allowed to spend, at most.
///
/// Two, which is Nyquist: below it adjacent gates share a texel and detail the
/// radar measured is lost, above it the picture is sampling its own
/// interpolation rather than any new measurement. It is a statement about
/// sampling and not a tuning knob, which is why it is spelt here once and
/// derived from nowhere.
pub const TEXELS_PER_SAMPLE: f64 = 2.0;

/// The largest side worth painting `extent_km` of a field sampled every
/// `sample_km` onto — the point past which more texels buy nothing.
///
/// # The two terms, and why neither alone is the answer
///
/// **The data's own term** is `TEXELS_PER_SAMPLE` per sample across the
/// diameter: `2 · extent_km / sample_km · TEXELS_PER_SAMPLE`. On a WSR-88D
/// surveillance cut — 1832 gates of 0.25 km, ±460.11 km on the ground — that
/// is 7362 px, and at 4096 the same cut gets **1.11** texels per gate, so the
/// display has been discarding half of what the radar measured out there for
/// as long as it has drawn past the base extent. Measured on this crate over
/// seven sites (KDMX, KCRP, KFTG, KATX, KPDT, KTLX, TORD).
///
/// **The display's own term** is the scale the base texture is calibrated at,
/// `IMAGE_SIZE / (2 · BASE_EXTENT_KM)` = 4.4522 px/km, applied to whatever
/// ground this raster covers. It is here because a radial Nyquist figure alone
/// would *lower* the side for a coarsely sampled field: a 1 km × 1° echo-tops
/// grid reaching 460 km asks for only 1840 px radially, and a raster that
/// narrow loses azimuthal detail inside about 50 km, where a 1° cell is far
/// finer than a range bin. Azimuthal cells shrink to nothing at the origin, so
/// no azimuthal Nyquist figure exists to bound them with; the display's
/// calibrated scale is the floor that stands in for it.
///
/// So the answer is the larger of the two, and the effect is exactly one
/// direction: **a raster is never coarser than the scale this display has
/// always drawn at, and rises above it only as far as the samples justify.**
///
/// # What each real sweep asks for, measured
///
/// | sweep | extent | sample | asks for | gets today |
/// |---|---:|---:|---:|---:|
/// | WSR-88D surveillance, 1832 gates | ±460.11 km | 0.25 km | 7362 | 4096 |
/// | WSR-88D Doppler, 1192 gates      | ±300.11 km | 0.25 km | 4802 | 4096 |
/// | TDWR long-range reflectivity     | ±417.00 km | 0.30 km | 5560 | 4096 |
/// | TDWR Doppler, 592 gates          | ±88.80 km  | 0.15 km | 2368 | 2048 |
/// | echo tops / VIL density / hail   | ±460 km    | 1.00 km | 4096 | 4096 |
///
/// The last two rows are why the floor is there and why the base branch of
/// [`raster_side_px`] is untouched: a TDWR Doppler disc is already inside
/// [`BASE_EXTENT_KM`], so it takes the base texture whatever this answers, and
/// the 1 km volume grids come out at the calibrated scale rather than under it.
///
/// A non-positive or non-finite `sample_km` says nothing about sampling, so it
/// answers the display's term alone rather than dividing by it.
pub fn data_limited_side_px(extent_km: f64, sample_km: f64) -> usize {
    let reference_scale_px_per_km = IMAGE_SIZE as f64 / (2.0 * BASE_EXTENT_KM);
    let diameter_km = 2.0 * extent_km.max(0.0);
    let at_reference = diameter_km * reference_scale_px_per_km;
    // `is_finite` before the comparison, not folded into it: every ordering
    // against a `NaN` is false, so `> 0.0` alone would admit one and the
    // division would carry it into the side.
    let at_nyquist = if sample_km.is_finite() && sample_km > 0.0 {
        diameter_km / sample_km * TEXELS_PER_SAMPLE
    } else {
        0.0
    };
    // `ceil` and not `round`: a side one texel short of the data's own limit is
    // still short of it.
    (at_reference.max(at_nyquist).ceil() as usize).max(1)
}

/// The horizontal-geodesy sphere and its degree ↔ kilometre conversion,
/// defined in `rustdar-geo` — the workspace's geometry floor, reached through
/// [`rustdar_source::geo`]'s re-export — and re-exported under the paths this
/// crate always published them at (`sites`, `render`, `xsect` and the voxel
/// builder all read them as `crate::types::`). The constants' own docs carry
/// the one-sphere reasoning and the 111.32 history;
/// `rustdar-radar/tests/geodesy_one_definition.rs` still guards against a
/// second spelling anywhere in the workspace.
pub use rustdar_source::geo::{EARTH_RADIUS_KM, KM_PER_DEGREE_LAT};

/// m/s to mph conversion factor.
pub const MS_TO_MPH: f32 = 2.23694;

/// The latitude Web Mercator ends at, defined in [`rustdar_source::geo`] —
/// the shared floor under this crate and `rustdar-overlays` — and re-exported
/// under the path this crate always published it at (`rustdar-egui`'s `tiles`
/// re-exports it from here). The constant's own doc carries the projection's
/// reasoning; `rustdar-radar/tests/geodesy_one_definition.rs` still guards
/// against a second spelling anywhere in the workspace.
pub use rustdar_source::geo::MERCATOR_LAT_LIMIT_DEG;

// Crate-visible re-exports so `render`'s gate loop and the types tests keep
// their spelling; the definitions (and their load-bearing `#[inline]`) live in
// `rustdar_source::geo`.
pub(crate) use rustdar_source::geo::{lat_rad_to_mercator_y, mercator_y_from_sin_lat};

/// Geographic bounds of the rendered radar image. Pixels are linearly spaced
/// in Web Mercator Y and longitude, matching slippy-map tile providers.
#[derive(Debug, Clone, Copy)]
pub struct ImageBounds {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
    pub mercator_y_min: f64,
    pub mercator_y_max: f64,
}

impl ImageBounds {
    /// Extent is `extent_km` in every direction from the site — the number
    /// [`plan_view_extent_km`] chose for this render, not a constant.
    ///
    /// It is a parameter and not a default because these bounds are the *only*
    /// statement of where a raster's pixels are: the frontend places the
    /// texture between the corners this returns, the hover reads a pixel back
    /// out of them, and the volume bridge reprojects the whole picture through
    /// them. A caller that built bounds at one extent for a raster painted at
    /// another would misplace every pixel by their ratio with nothing to
    /// notice it, so there is no argument-free way to get bounds at all: the
    /// render that made the picture reports its extent as `max_range_km`, and
    /// every placement site hands that back here.
    ///
    /// On [`KM_PER_DEGREE_LAT`], which is [`EARTH_RADIUS_KM`] — the same
    /// sphere [`crate::render::render_gate`] paints the gates inside these
    /// bounds on. It read `111.32` until the two were unified; see that
    /// constant for what moved. The two changes compose: the ratio is the
    /// sphere's and the multiplier is the render's, so a frame drawn at a
    /// TDWR's 417 km is placed on the same planet as a 230 km one.
    pub fn from_radar_site(radar_lat: f64, radar_lon: f64, extent_km: f64) -> Self {
        let radar_lat_rad = radar_lat.to_radians();
        let lat_deg_per_km = 1.0 / KM_PER_DEGREE_LAT;
        let lon_deg_per_km = 1.0 / (KM_PER_DEGREE_LAT * radar_lat_rad.cos());

        let max_lat_offset = extent_km * lat_deg_per_km;
        let max_lon_offset = extent_km * lon_deg_per_km;

        let min_lat = radar_lat - max_lat_offset;
        let max_lat = radar_lat + max_lat_offset;

        ImageBounds {
            min_lat,
            max_lat,
            min_lon: radar_lon - max_lon_offset,
            max_lon: radar_lon + max_lon_offset,
            mercator_y_min: lat_rad_to_mercator_y(min_lat.to_radians()),
            mercator_y_max: lat_rad_to_mercator_y(max_lat.to_radians()),
        }
    }
}

/// The geographic half of an [`ImageBounds`]: the four lat/lon edges, copied.
/// The mercator pair is placement arithmetic, re-derivable through
/// `lat_rad_to_mercator_y`, so nothing is lost that cannot be recomputed.
impl From<ImageBounds> for rustdar_source::geo::GeoBounds {
    fn from(bounds: ImageBounds) -> Self {
        Self {
            min_lat: bounds.min_lat,
            max_lat: bounds.max_lat,
            min_lon: bounds.min_lon,
            max_lon: bounds.max_lon,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScanInfo {
    /// Where this volume's radar is, and how high.
    ///
    /// **Not simply the table row.** The row is the starting point; a volume
    /// that states its own position overrides it, and a position learned from
    /// an earlier volume overrides it in a session where no fresh volume has
    /// arrived. [`ScanInfo::site_source`] says which of the three this is.
    pub site: RadarSite,
    /// Which of the three things above [`ScanInfo::site`] came from.
    ///
    /// The one thing every consumer of [`ScanInfo::site`] can use to tell a
    /// measured position from a placeholder:
    /// [`SitePositionSource::Unknown`] means the coordinates on the site are
    /// not an answer at all.
    pub site_source: SitePositionSource,
    /// The canonical integer position behind [`ScanInfo::site`], when there is
    /// one.
    ///
    /// `None` for [`SitePositionSource::Table`] and
    /// [`SitePositionSource::Unknown`] — the table's rows are `f64` literals
    /// and there is nothing measured to remember. `Some` for the other two,
    /// and the caller persists it when [`ScanInfo::site_source`] is
    /// [`SitePositionSource::Volume`]: that is the moment something was
    /// *learned*, as opposed to recalled.
    pub site_position: Option<SitePosition>,
    /// From the **first** radial of the **first** sweep, not the request.
    ///
    /// # Not a freshness signal on the live chunk feed
    ///
    /// On the archive path a volume arrives whole, so this moves once per volume
    /// and is a sound key for "is what is on screen still the truth?". On the
    /// live chunk feed the `Scan` grows sweep by sweep with `sweeps[0]` fixed, so
    /// this is a **constant for the whole five-to-six minute volume** while the
    /// tilt ladder underneath it goes from one rung to fourteen. Anything that
    /// wants to notice a live volume filling has to look at the volume, not at
    /// this — see `SectionTarget::sweeps` in `rustdar-egui`, which is the
    /// discriminator a cross-section pane uses and the second attempt at one.
    pub timestamp: NaiveDateTime,
    /// Volume Coverage Pattern number (e.g. 212, 215, 35)
    pub vcp_number: u16,
    pub available_products: Vec<RadarProduct>,
    /// Elevation angles per product, sorted ascending.
    ///
    /// **Accumulated by the UI, not a property of one volume.** `ScanInfo` is
    /// rebuilt per chunk round, but `Gui::apply_chunk_scan_info` *merges* the
    /// fresh angles into the pane's existing set and never removes one; only a
    /// completed volume replaces it wholesale. So mid-volume this can hold angles
    /// the `Scan` in hand does not carry, and after a session's first complete
    /// volume it already holds every angle the VCP flies. It answers "what can
    /// this site show?", which is what the product and tilt pickers want. It does
    /// **not** answer "how much of this volume has arrived?", and using it for
    /// that is a bug that only appears on the second volume of a session.
    pub product_elevations: HashMap<RadarProduct, Vec<f32>>,
    pub status: String,
}

/// Whether the fetched catalogue agrees that `site` is where `stated` puts it,
/// or has nothing to say about `site` at all.
///
/// The one thing standing between a volume's two unchecked `Real*4` coordinate
/// fields and a position that outranks every other source and is then
/// persisted. See [`ScanInfo::from_scan`] for the rule and
/// [`CATALOGUE_DISAGREEMENT_LIMIT_KM`] for the measurement behind the distance.
///
/// `true` when the catalogue cannot speak, which is the honest answer rather
/// than a lenient one: a radar the catalogue has never placed has no second
/// opinion to be confirmed by, and refusing it a position would leave it with
/// none at all.
///
/// Pinned by `a_volume_that_disagrees_with_the_catalogue_does_not_displace_it`.
fn confirmed_by_catalogue(site: &str, stated: &SitePosition) -> bool {
    let Some((lat, lon)) = crate::sites::catalogue_position(site) else {
        return true;
    };
    let apart_km = crate::sites::distance_km(stated.lat(), stated.lon(), lat, lon);
    if apart_km > CATALOGUE_DISAGREEMENT_LIMIT_KM {
        // Error, not warning. Every reachable cause is something somebody
        // needs to see: a corrupt Volume Data Block, a producer writing a
        // scale nothing here recognises, or a radar that genuinely relocated
        // and whose catalogue entry has not caught up. The last of those is
        // the only benign one and it is still a thing to act on.
        log::error!(
            "volume for {site} states ({:.5}, {:.5}), {apart_km:.1} km from where the \
             catalogue places it ({lat:.5}, {lon:.5}); keeping the catalogue's position",
            stated.lat(),
            stated.lon(),
        );
        return false;
    }
    true
}

impl ScanInfo {
    /// Level III products are listed with empty elevation vectors, filled in
    /// later as L3 data arrives.
    ///
    /// # The site's position: volume, then learned, then table
    ///
    /// This is the one place in the workspace where a radar's position is
    /// decided, and the precedence is the design. It is pinned as a table by
    /// `the_precedence_is_volume_then_learned_then_table`.
    ///
    /// 1. **The volume in hand.** Every Message 31 volume states its own
    ///    latitude, longitude and heights in its Volume Data Block, and
    ///    `crate::scan::decoded` has always read them — but until this
    ///    existed, `Scan::site()` had no caller anywhere in the workspace and
    ///    the value was decoded and dropped. Preferring it makes every site
    ///    the user actually opens self-correcting, with no network and no new
    ///    origin.
    ///
    ///    Last-writer-wins, with no averaging and no outlier policy, because
    ///    the reported position does not wobble: across 18 diverse sites at
    ///    2019, 2022 and 2026 it is bit-identical, span 0.0 m. Where it moves
    ///    it is a step function — `KTLX` made one 43 m re-survey step between
    ///    2013 and 2016 — so a disagreement means a re-survey happened and the
    ///    newer value is the right one.
    ///
    ///    **Within a kilometre of the fetched catalogue, and not otherwise.**
    ///    A radar reporting itself outranks a record about it by metres, which
    ///    is the scale radars actually move at; it does not get to outrank it
    ///    by kilometres. The volume's coordinates are two `Real*4` fields with
    ///    no checksum over them, and one of the two readings this workspace
    ///    takes of them — `nexrad_decode`'s thousandths repair, for the older
    ///    TDWR producer — is an inference that damage in those same fields can
    ///    satisfy: `(100, -100)` is not a position, divides into one, and
    ///    would place the radar in the Gulf of Guinea. Nothing inside the
    ///    volume can tell that apart from a real repair, so it is told apart
    ///    here, against the one source no volume wrote. See
    ///    [`CATALOGUE_DISAGREEMENT_LIMIT_KM`] for where the kilometre comes
    ///    from — it is measured, and it sits in a band four orders of magnitude
    ///    wide that the archive puts nothing in.
    ///
    ///    The check is skipped, not failed, when no catalogue has placed the
    ///    radar: a fresh install before its first fetch, and the two
    ///    identifiers with real Level II data that `api.weather.gov` 404s
    ///    (`TPBI`, `KCRI`). Those keep the volume's word. That is the cost of
    ///    confirming against an outside source — where there is no outside
    ///    source there is no confirmation — and it is preferred to the
    ///    alternative of refusing them a position at all, which would take the
    ///    whole terminal-radar repair back off `TPBI`.
    ///
    /// 2. **A position learned from an earlier volume**, supplied by the
    ///    caller out of its own store. This is what makes a site stay
    ///    corrected across restarts, and what lets the map centre correctly on
    ///    a site opened before but not yet re-downloaded this session.
    ///
    /// 3. **[`crate::sites::radars()`]**, whatever this process has resolved.
    ///    Still the answer for a pre-2010 `AR2V0001` volume, which is Message 1
    ///    throughout and carries no Volume Data Block to read — from either
    ///    source, since a chunk-fed `Scan` reads the same block through
    ///    [`crate::chunks::ChunkContents::site`] and reaches rung 1 with it.
    ///
    /// A site none of the three can place gets
    /// [`SitePositionSource::Unknown`] and a placeholder row. See
    /// [`crate::sites::UNKNOWN_SITE_NAME`] for why that row is not an answer.
    pub fn from_scan(
        data: &Scan,
        site: &str,
        requested_timestamp: NaiveDateTime,
        learned: Option<SitePosition>,
    ) -> Self {
        let vcp_number = data.coverage_pattern_number().number();

        // Resolved before discovery, not after: what a site's network *is*
        // decides which products can be offered for it at all, so
        // `discover_product_elevations` has to be handed the row rather than
        // the row being looked up afterwards for its coordinates. Nothing in
        // the precedence below reads a product or a timestamp, so it is free
        // to move up here.
        let row = get_radar_site(site);
        let stated = data
            .site()
            .and_then(SitePosition::from_volume)
            .filter(|stated| confirmed_by_catalogue(site, stated));
        let (site_position, site_source) = match (stated, learned, row.is_some()) {
            (Some(volume), _, _) => (Some(volume), SitePositionSource::Volume),
            (None, Some(learned), _) => (Some(learned), SitePositionSource::Learned),
            (None, None, true) => (None, SitePositionSource::Table),
            (None, None, false) => (None, SitePositionSource::Unknown),
        };

        // A radar this process knows of and cannot place has no `row` and is
        // still not anonymous: the catalogue listed its identifier, and
        // `sites` leaked it. Naming it here is what keeps `is_tdwr` right for
        // `TPBI` — a terminal radar with real Level II data that
        // `api.weather.gov` will not place — which the compiled-in table used
        // to settle by placing it. Without this it is named
        // `UNKNOWN_SITE_NAME`, `is_wsr88d` answers true, and the picker offers
        // four Level III products its SPG does not generate.
        let known_name = crate::sites::static_name(site);
        let radar_site = match (site_position, row) {
            (Some(position), Some(row)) => position.applied_to(Some(row)),
            (Some(position), None) => position
                .applied_to_named(known_name.unwrap_or(crate::sites::UNKNOWN_SITE_NAME), None),
            (None, Some(row)) => row.clone(),
            (None, None) => {
                // Error, not warning: nothing downstream of here can place
                // this pane, and every number it draws — the range rings, the
                // gate positions, the hover readout, the section endpoints —
                // is about a spot in the Gulf of Guinea rather than about a
                // radar. `radar_height_ft_near` refuses to answer for it.
                log::error!(
                    "no position for radar site '{site}': it is in no table row, \
                     its volume states none, and nothing was learned for it",
                );
                RadarSite {
                    name: known_name.unwrap_or(crate::sites::UNKNOWN_SITE_NAME),
                    lat: 0.0,
                    lon: 0.0,
                    heights: None,
                }
            }
        };

        let product_elevations = discover_product_elevations(data, &radar_site);

        let mut available_products: Vec<RadarProduct> =
            product_elevations.keys().copied().collect();
        available_products.sort_by_key(|p| p.sort_order());

        let actual_timestamp = data
            .sweeps()
            .first()
            .and_then(|s| s.radials().first())
            .and_then(|r| {
                chrono::DateTime::from_timestamp_millis(r.collection_timestamp())
                    .map(|dt| dt.naive_utc())
            })
            .unwrap_or(requested_timestamp);

        let status = format!(
            "Loaded {} products: {}",
            available_products.len(),
            available_products
                .iter()
                .map(|p| p.name())
                .collect::<Vec<_>>()
                .join(", ")
        );

        ScanInfo {
            site: radar_site,
            site_source,
            site_position,
            timestamp: actual_timestamp,
            vcp_number,
            available_products,
            product_elevations,
            status,
        }
    }
}

/// Rounds elevation angles to 0.1° so SAILS/MRLE repeat scans and split cuts
/// at the same nominal angle collapse to one entry.
///
/// The angle is the sweep's **median**
/// ([`crate::volumetric::sweep_elevation_deg`]), not its first radial's. These
/// are the labels the picker shows and the values `render::find_sweep` is later
/// handed to find the sweep again, so naming a tilt by a radial taken while the
/// antenna was still settling produced entries that drew a different cut from
/// the one on the label — and, where two labels collapsed onto one sweep, cuts
/// the picker could not reach at all. `find_sweep` matches on the same median,
/// so an entry and the sweep behind it are the same quantity.
///
/// # What is offered is what can be drawn
///
/// The map this returns *is* the product picker, and `ScanInfo` accumulates
/// downstream — `Gui::apply_chunk_scan_info` merges and never removes — so an
/// entry that cannot render has to be withheld here or it is permanent for the
/// session. Two things are withheld, both decided per **volume** and per
/// **site** rather than per sweep: the hybrid classification on a single-pol
/// volume, and the Level III family at a site whose network has no RPG.
fn discover_product_elevations(scan: &Scan, site: &RadarSite) -> HashMap<RadarProduct, Vec<f32>> {
    let mut product_elevations: HashMap<RadarProduct, Vec<f32>> = HashMap::new();

    // Asked once of the volume, not once per sweep.
    // [`RadarProduct::HydrometeorClassification`] lists off the *reflectivity*
    // slot ([`RadarProduct::moment_slot`]) because it composites every dual-pol
    // tilt into one tilt-independent field — so a per-sweep test would offer it
    // from a split cut's dual-pol Doppler half and withdraw it again from the
    // surveillance half, the entry flapping as a live volume filled. One sweep
    // carrying both of the moments `crate::hhc` cannot run without is enough to
    // answer for the whole volume.
    let volume_is_dual_pol = scan.sweeps().iter().any(|sweep| {
        sweep.radials().first().is_some_and(|radial| {
            radial.differential_phase().is_some() && radial.correlation_coefficient().is_some()
        })
    });

    for (i, sweep) in scan.sweeps().iter().enumerate() {
        if let Some(first_radial) = sweep.radials().first() {
            let raw_angle = crate::volumetric::sweep_elevation_deg(sweep.radials())
                .unwrap_or_else(|| f64::from(first_radial.elevation_angle_degrees()));
            let elev_angle = (raw_angle * 10.0).round() as f32 / 10.0;

            let mut products_found: Vec<&str> = Vec::new();
            for product in RadarProduct::all() {
                // The one product whose moment slot does not stand for the data
                // it reads: reflectivity is where it *lists*, ΦDP and ρHV are
                // what it classifies from. On a single-pol volume — every TDWR
                // volume, and every legacy Message 1 WSR-88D one — `crate::hhc`
                // refuses cleanly and the pane stays empty, so listing it beside
                // the reflectivity tilts offers a product that can only ever
                // draw nothing.
                if *product == RadarProduct::HydrometeorClassification && !volume_is_dual_pol {
                    continue;
                }
                if product.get_moment(first_radial).is_some() {
                    products_found.push(product.code());
                    product_elevations
                        .entry(*product)
                        .or_default()
                        .push(elev_angle);
                }
            }
            log::info!(
                "  Sweep {:2}: raw={:.2}° rounded={:.1}° radials={} products=[{}]",
                i,
                raw_angle,
                elev_angle,
                sweep.radials().len(),
                products_found.join(", ")
            );
        } else {
            log::warn!("  Sweep {:2}: no radials!", i);
        }
    }

    for angles in product_elevations.values_mut() {
        angles.sort_by(|a, b| a.total_cmp(b));
        angles.dedup();
    }
    for (product, angles) in &product_elevations {
        log::info!(
            "  {} → {} unique elevations: {:?}",
            product.code(),
            angles.len(),
            angles
        );
    }

    // Level III objects are made by an RPG, and only the WSR-88D network has
    // one. A TDWR is served by the Supplemental Product Generator, which
    // publishes its own short list and none of the four objects
    // [`RadarProduct::level3_products`] names. Measured against the bucket the
    // fetch itself reads, on 2026-08-11, for `TPIT`'s three-letter form:
    //
    //     curl -s "https://unidata-nexrad-level3.s3.amazonaws.com/\
    //              ?list-type=2&prefix=PIT_&delimiter=_&max-keys=200"
    //
    // returned a complete listing (`IsTruncated false`) of twenty codes — DHR,
    // DPA, DSP, N1P, NCR, NET, NHI, NMD, NST, NTV, NVL, NVW, RSL, TV0-TV2,
    // TZ0-TZ2, TZL — and not one of N0K/EET/DVL/DPR. `PIT_TZL_2026_08_11_…`
    // keys exist, so the site is archived and current; these products are
    // simply not generated for it. Offering them anyway put five entries in the
    // picker that draw an empty pane forever, and — because `ScanInfo`
    // accumulates — they stayed there for the rest of the session.
    //
    // `is_wsr88d` answers **true** for the unplaceable-site row `from_scan`
    // builds (it is named [`crate::sites::UNKNOWN_SITE_NAME`], not the `T`
    // prefix `is_tdwr` looks for), and for the row a volume's own position
    // builds when [`crate::sites::radars()`] has no entry to name it from —
    // `SitePosition::applied_to` reaches for the same constant. So a site the
    // resolved table has never heard of keeps every product it is offered
    // today, and only a site the table does name as a `T` loses the four.
    if site.is_wsr88d() {
        for l3_product in RadarProduct::all().iter().filter(|p| p.is_level3()) {
            product_elevations.entry(*l3_product).or_default();
        }
    }

    product_elevations
}

/// A Level II moment field on a [`Radial`], named rather than read.
///
/// Several products share one: NROT is derived from velocity, and interpolated
/// echo tops from reflectivity. Naming the field — instead of only being able
/// to fetch it — is what lets a moment be put *back* onto a radial, which
/// [`crate::render_input`] does when it rebuilds a scan from a payload.
///
/// Deliberately a smaller set than [`RadarProduct`]: the Level III products
/// have no Level II field at all, which is what
/// [`RadarProduct::moment_slot`]'s `None` means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MomentSlot {
    Reflectivity,
    Velocity,
    SpectrumWidth,
    DifferentialReflectivity,
    DifferentialPhase,
    CorrelationCoefficient,
}

impl MomentSlot {
    /// This field's value on `radial`.
    pub fn read<'a>(&self, radial: &'a Radial) -> Option<&'a nexrad_model::data::MomentData> {
        match self {
            MomentSlot::Reflectivity => radial.reflectivity(),
            MomentSlot::Velocity => radial.velocity(),
            MomentSlot::SpectrumWidth => radial.spectrum_width(),
            MomentSlot::DifferentialReflectivity => radial.differential_reflectivity(),
            MomentSlot::DifferentialPhase => radial.differential_phase(),
            MomentSlot::CorrelationCoefficient => radial.correlation_coefficient(),
        }
    }

    /// The product that **is** this moment, rather than one computed from it.
    ///
    /// The inverse of the six identity rows of [`RadarProduct::moment_slot`],
    /// and the reason [`RadarProduct::is_wire_moment`] can be written without a
    /// second copy of the list: a product is its own moment exactly when its
    /// slot points back at it. The other eleven products also have slots —
    /// normalized rotation reads velocity, the hail pair read reflectivity —
    /// and those rows are the ones that must not round-trip.
    pub fn product(&self) -> RadarProduct {
        match self {
            MomentSlot::Reflectivity => RadarProduct::Reflectivity,
            MomentSlot::Velocity => RadarProduct::Velocity,
            MomentSlot::SpectrumWidth => RadarProduct::SpectrumWidth,
            MomentSlot::DifferentialReflectivity => RadarProduct::DifferentialReflectivity,
            MomentSlot::DifferentialPhase => RadarProduct::DifferentialPhase,
            MomentSlot::CorrelationCoefficient => RadarProduct::CorrelationCoefficient,
        }
    }
}

/// Why a cell of a decoded grid has no number — or, for [`GateReport::Value`],
/// that it has one.
///
/// The decoder answers a gate query four ways and a dense `f64`/`f32` grid can
/// only write one of them, so the other three arrive as the same `NaN` and the
/// consumer cannot tell them apart. They are not the same fact:
///
/// * [`BelowThreshold`](Self::BelowThreshold) is a **measurement**. The radar
///   illuminated that gate and found nothing above the moment's signal
///   threshold. "Empty" is what it observed.
/// * [`RangeFolded`](Self::RangeFolded) is also a measurement, and the
///   *opposite* one: there is signal, and only its range is ambiguous.
/// * [`NotReported`](Self::NotReported) is the sole genuine absence — no gate
///   exists there to have said anything.
///
/// An occupancy rule, a stencil that demands intact taps, and a column scan
/// that has to decide where an echo stops all want to weigh those differently,
/// and none of them can while the three share a bit pattern.
///
/// # Relationship to the two neighbouring types
///
/// The first three arms mirror `nexrad_model::data::MomentValue`'s own, which
/// is the point: this is that enum with its `f32` split off into a parallel
/// plane, plus the fourth case a *grid* has and a single gate does not.
///
/// [`crate::sampler::SampleStatus`] is the richer cousin and stays separate.
/// Four of its seven arms — `BelowLowestBeam`, `AboveVolume`, `BeyondRange`,
/// `NoCoverage` — describe where a *query* fell in a ladder, which a decoded
/// grid has no way to be and no business claiming. Reusing it here would let a
/// grid cell answer `BelowLowestBeam`, so it does not.
///
/// # Ordering is precedence, and it is load-bearing
///
/// [`Ord`] is derived over the declaration order below, so `max` is the rule
/// for collapsing several gates into one cell: a measured number beats
/// ambiguous signal, ambiguous signal beats measured emptiness, and measured
/// emptiness beats no gate at all. Reordering the variants silently changes
/// what every aggregating grid reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum GateReport {
    /// No gate covered this cell: the radial carried no such moment, the cell
    /// is past the moment's last gate, or no radial served the azimuth. The
    /// default, because a grid starts out having been told nothing.
    #[default]
    NotReported = 0,
    /// Every gate under this cell was below the moment's signal threshold
    /// (raw code 0). The radar looked and saw nothing — a measurement of
    /// absence, not an absence of measurement.
    BelowThreshold = 1,
    /// A gate under this cell was range folded (raw code 1) and none carried a
    /// value: signal is present, and only its range is ambiguous past the
    /// unambiguous range of the cut's PRF.
    RangeFolded = 2,
    /// At least one gate under this cell carried a number, so the grid's own
    /// value is defined here.
    Value = 3,
}

impl GateReport {
    /// What one `MomentValue` reports, before any cell aggregation.
    pub fn of(value: &nexrad_model::data::MomentValue) -> Self {
        match value {
            nexrad_model::data::MomentValue::Value(_) => Self::Value,
            nexrad_model::data::MomentValue::BelowThreshold => Self::BelowThreshold,
            nexrad_model::data::MomentValue::RangeFolded => Self::RangeFolded,
        }
    }

    /// Whether the radar *looked* at this cell, whatever it found.
    ///
    /// True for all three of the decoder's answers and false only for
    /// [`NotReported`](Self::NotReported). This is the question a consumer
    /// deciding "is this gap real data or missing data" is actually asking,
    /// and naming it keeps that question from being spelt three different
    /// ways at three call sites.
    pub fn is_measured(self) -> bool {
        self != Self::NotReported
    }
}

/// What a render *draws*, as opposed to what it draws it of.
///
/// Three products of one moment can share a renderer; three views of one
/// product cannot share a raster. A plan view is `IMAGE_SIZE²` of ground, a
/// section is [`crate::xsect::SECTION_WIDTH`] × [`crate::xsect::SECTION_HEIGHT`]
/// of a vertical plane, and a volume is a 3D index grid — different shapes,
/// different buffers, and nothing in a buffer says which it is.
///
/// It lives here, in the crate both the frontend and the UI depend on, so
/// `rustdar_egui`'s `PaneContent` can map *into* it without either of those
/// crates having to name the other. A pane *kind* is what a pane is; this is
/// what a render produced, and the two are deliberately **not** one-to-one: a
/// map pane produces a `PlanView` or a `Volume` depending on its render mode,
/// which is exactly what makes 3D an alternative rendering of a pane rather
/// than another kind of pane. A pane is a place on screen with state and a
/// lifetime; a `RenderView` is a fact about a buffer that outlives the pane
/// that asked for it — it is what a cached render is keyed by, and it is
/// therefore also what looping and whole-volume reads are classified against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderView {
    /// The plan-view raster every render produced before cross-sections
    /// existed.
    PlanView,
    /// A vertical slice along a line.
    CrossSection,
    /// A resampled Cartesian grid, for a raymarch.
    Volume,
}

impl RenderView {
    /// Whether a render of this view reads every tilt carrying the moment,
    /// rather than the one sweep `crate::render::find_sweep` picks.
    ///
    /// The *view*-side half of the whole-volume question;
    /// [`RadarProduct::reads_whole_volume`] is the product-side half. Both have
    /// to be asked, and neither can answer for the other: a reflectivity
    /// cross-section answers **no** to the product question — it is the same
    /// moment the plan view rasterizes — and **yes** to this one. A dispatch
    /// that asked only the product question would hand a section a scan whose
    /// cuts had been deliberately skipped, and a section of a partial volume
    /// does not fail and does not produce a `NaN`: it interpolates across the
    /// gap and draws a smooth layer that is not there, which looks *better*
    /// than the truth.
    ///
    /// Exhaustive, like [`RadarProduct::reads_whole_volume`]: a fourth view
    /// fails to compile until it has been classified. `!matches!(self,
    /// PlanView)` would classify a new view as whole-volume on its own, which
    /// is the safe direction, but a view that really did read one tilt would
    /// then silently widen every download its pane triggers.
    pub fn reads_whole_volume(self) -> bool {
        match self {
            Self::PlanView => false,
            // A section interpolates between the tilts bracketing each sample
            // by beam height; a raymarch reads a grid resampled from every cut.
            // Both are vertical structure, which one sweep does not have.
            Self::CrossSection | Self::Volume => true,
        }
    }

    /// Whether a pane producing this view can animate a sequence of past
    /// volumes.
    ///
    /// A loop is a sequence of *rendered pictures*, one per volume, held as
    /// textures — so the question is not "does this view draw radar" but "can
    /// one volume's worth of it be reduced to a picture that stays correct
    /// while it sits in a list". All three can:
    ///
    /// * A plan view is an `IMAGE_SIZE²` raster of one tilt, positioned by the
    ///   site's coordinates. Nothing about the pane changes what it depicts.
    /// * A cross-section is a [`crate::xsect::SECTION_WIDTH`] ×
    ///   [`crate::xsect::SECTION_HEIGHT`] raster of one line through one
    ///   volume. The line is part of the loop's identity, exactly as the
    ///   product is for a plan view.
    /// * A **3D volume** can too, and its frame is the one that is not a
    ///   picture. The picture is raymarched live from the eye every frame, so a
    ///   cached *image* would be specific to the camera and one orbit would
    ///   invalidate the whole loop at once. What it caches instead is the
    ///   **input**: each frame is a resident 3D texture and the march swaps
    ///   which one it samples, at a measured +0.01 ms (+2%) on a discrete GPU
    ///   and +0.31–0.78 ms (+3–4%) on a software rasteriser. So orbiting a
    ///   resident loop costs nothing, and a frame's identity is a volume target
    ///   rather than a raster.
    ///
    /// **Classified against the view rather than the pane kind**, because the
    /// answer is a property of what a frame *is*, and a map pane produces two
    /// different kinds of frame depending on its render mode. Asking the kind
    /// would give one answer for both.
    ///
    /// Exhaustive on purpose, like [`Self::reads_whole_volume`]: a fourth view
    /// must be classified here rather than defaulting into — or out of — the
    /// loop machinery. The direction matters, because the two mistakes are not
    /// symmetric. A view wrongly excluded is a missing feature; a view wrongly
    /// included is a pane whose frames nothing renders, which under Sync Layers
    /// holds **every other pane's** loop back for ever. That asymmetry is why
    /// `Volume` answered `false` until three things existed: a store a holder
    /// can own a *set* of grids in, a build path that accepts a volume time
    /// that is not the newest, and a pacing budget for the resample. All three
    /// do now, which is what changed the answer — the claim was never that the
    /// memory did not fit.
    pub fn can_loop(self) -> bool {
        match self {
            Self::PlanView | Self::CrossSection | Self::Volume => true,
        }
    }

    /// Whether the pane's **selected elevation** chooses which picture a render
    /// of this view showing `product` produces — and therefore whether anything
    /// holding such a render has to key on the tilt.
    ///
    /// `false` means the tilt is not part of that render's identity: two
    /// renders of one `(site, product, view)` at different selections are the
    /// same bytes, so a cache may collapse them into one slot and a loop may
    /// keep its frames across a tilt click.
    ///
    /// **Only a plan view has a tilt to ask about, and only for some products.**
    ///
    /// * **A cross-section** cuts across every rung of the ladder, so there is
    ///   no selection to answer for. The pipeline says so at three separate
    ///   points rather than by convention: [`crate::xsect::SectionRequest`] is
    ///   `(start, end, top, product)` with no elevation field;
    ///   [`RenderInput::extract_volume_parts`] — the only door a section
    ///   payload comes through — stores [`NO_ELEVATION_DEG`] rather than the
    ///   caller's angle, and takes no angle to store; and
    ///   [`crate::xsect::render_section`] reaches the sampler through
    ///   [`crate::derive::prepare`], which derives per sweep across the whole
    ///   ladder and never calls `render::find_sweep`. That last point is what
    ///   makes the answer hold for **NROT and SRV too**: those two rasterize
    ///   the sweep `find_sweep` picks in a *plan* view, which is why they are
    ///   tilt-dependent there, but the section path does not run that
    ///   rasterizer at all.
    /// * **A voxel grid** is resampled from the whole ladder for the same
    ///   reason, which is why [`crate::render_input::NO_ELEVATION_DEG`] serves
    ///   both vertical views.
    /// * **A plan view** rasterizes one sweep — unless the product is one
    ///   [`RadarProduct::tilt_independent_plan_view`] names, which reduce the
    ///   whole volume before `render::render_radar_to_image_full` ever calls
    ///   `find_sweep`.
    ///
    /// # One predicate, because two copies of it already disagreed
    ///
    /// `rustdar_frontend`'s `render_cache_key` and `rustdar_egui`'s
    /// `LoopPlaybackState::retarget_renders_keyed` both ask this. They used to
    /// answer it separately, and they disagreed in both directions: the loop
    /// charged a tilt click for the four whole-volume plan views the cache
    /// already collapsed, *and* charged a section loop for a tilt no section
    /// can see — up to `MAX_LOOP_RENDER_BUDGET` re-renders apiece, none of
    /// which consult that cache. Classified against the **view**, not the pane
    /// kind, for [`can_loop`](Self::can_loop)'s reason: one map pane produces
    /// two different kinds of frame depending on its render mode.
    ///
    /// [`RenderInput::extract_volume_parts`]: crate::render_input::RenderInput::extract_volume_parts
    /// [`NO_ELEVATION_DEG`]: crate::render_input::NO_ELEVATION_DEG
    pub fn elevation_selects_picture(self, product: RadarProduct) -> bool {
        match self {
            Self::PlanView => !product.tilt_independent_plan_view(),
            Self::CrossSection | Self::Volume => false,
        }
    }

    /// A stable byte for the wire and for a cache key, **not** the declaration
    /// order.
    ///
    /// Same discipline as [`RadarProduct::wire_code`]: reordering the variants
    /// must not silently change what a stored key or a posted job means.
    pub fn wire_code(self) -> u8 {
        match self {
            Self::PlanView => 1,
            Self::CrossSection => 2,
            Self::Volume => 3,
        }
    }

    /// The view a [`wire_code`](Self::wire_code) names, or `None` for a byte
    /// this build does not have — the two ends of a worker port can be
    /// different builds.
    pub fn from_wire_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::PlanView),
            2 => Some(Self::CrossSection),
            3 => Some(Self::Volume),
            _ => None,
        }
    }

    /// Every view there is, for the sweeps that have to cover all of them.
    pub fn all() -> &'static [RenderView] {
        &[
            RenderView::PlanView,
            RenderView::CrossSection,
            RenderView::Volume,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum RadarProduct {
    Reflectivity,
    Velocity,
    SpectrumWidth,
    DifferentialPhase,
    CorrelationCoefficient,
    DifferentialReflectivity,
    StormRelativeVelocity,
    SpecificDifferentialPhase,
    EchoTops,
    EchoTopsInterpolated,
    VerticallyIntegratedLiquid,
    VilDensity,
    ProbabilityOfSevereHail,
    MaxExpectedHailSize,
    HydrometeorClassification,
    PrecipitationRate,
    NormalizedRotation,
}

impl RadarProduct {
    pub fn code(&self) -> &'static str {
        crate::product_spec::spec(*self).code
    }

    pub fn name(&self) -> &'static str {
        crate::product_spec::spec(*self).name
    }

    pub fn all() -> &'static [RadarProduct] {
        &[
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::SpectrumWidth,
            RadarProduct::DifferentialPhase,
            RadarProduct::CorrelationCoefficient,
            RadarProduct::DifferentialReflectivity,
            RadarProduct::StormRelativeVelocity,
            RadarProduct::SpecificDifferentialPhase,
            RadarProduct::EchoTops,
            RadarProduct::EchoTopsInterpolated,
            RadarProduct::VerticallyIntegratedLiquid,
            RadarProduct::VilDensity,
            RadarProduct::ProbabilityOfSevereHail,
            RadarProduct::MaxExpectedHailSize,
            RadarProduct::HydrometeorClassification,
            RadarProduct::PrecipitationRate,
            RadarProduct::NormalizedRotation,
        ]
    }

    /// Order products are listed in the UI.
    pub fn sort_order(&self) -> u8 {
        crate::product_spec::spec(*self).sort_order
    }

    pub fn is_level3(&self) -> bool {
        crate::product_spec::spec(*self).is_level3
    }

    /// The AWIPS product IDs to fetch for this product. These key the
    /// `unidata-nexrad-level3` bucket (`TLX_N0S_2026_07_25_...`). `None` for
    /// Level II products.
    ///
    /// Usually one per tilt, and usually one entry. VIL density is the
    /// exception: it is **derived from two objects**, `DVL` over `EET` for the
    /// same volume ([`crate::vild`]), so it names both — the only product here
    /// whose codes are inputs to a computation rather than tilts of itself, and
    /// the only one that reuses codes another product also fetches.
    ///
    /// Storm-relative velocity is deliberately absent: it once fetched five
    /// objects here — `N0S` for the vector in its PDB and `N0G`/`N1G`/
    /// `N2U`/`N3U` as dealiased tilts — and is now derived entirely from the
    /// Level II volume already in hand, dealiased locally and shifted by
    /// [`crate::srv::storm_motion`]'s chain — which still reads an `N0S` for
    /// the RPG's own vector, on the Level III round the app already makes.
    pub fn level3_products(&self) -> Option<&'static [&'static str]> {
        crate::product_spec::spec(*self).level3_codes
    }

    /// Every product whose [`level3_products`](Self::level3_products) names
    /// `code` — the inverse of that table, derived from it rather than written
    /// out a second time.
    ///
    /// One object can serve several products, and since VIL density arrived
    /// [it does](Self::level3_products): `DVL` is both
    /// `VerticallyIntegratedLiquid`'s whole field and VIL density's numerator,
    /// and `EET` is both `EchoTops`' field and its denominator. A fetched object
    /// therefore belongs to a *code*, not to one product, and everything that
    /// used to key on the product it was fetched "for" — which pane to redraw,
    /// which entries to add to the product picker — has to ask this instead.
    ///
    /// In [`sort_order`](Self::sort_order) order, so a caller that renders the
    /// answer produces the same list every time.
    pub fn level3_readers(code: &str) -> Vec<RadarProduct> {
        let mut readers: Vec<RadarProduct> = Self::all()
            .iter()
            .copied()
            .filter(|p| {
                p.level3_products()
                    .is_some_and(|codes| codes.contains(&code))
            })
            .collect();
        readers.sort_by_key(|p| p.sort_order());
        readers
    }

    /// The distinct AWIPS objects `products` need between them, each named once.
    ///
    /// What one site poll fetches. [`level3_products`](Self::level3_products) is
    /// a per-product table and two products may name the same object, so walking
    /// it product by product asks the bucket for the same ~100 KB twice per poll
    /// — `DVL` for VIL and again for VIL density, `EET` for echo tops and again
    /// for VIL density. De-duplicated here, in one place, so the fetch loop and
    /// the object cache agree on what "one object" is.
    ///
    /// Sorted, so a poll dispatches in the same order every run.
    pub fn level3_codes_for(products: &[RadarProduct]) -> Vec<&'static str> {
        let mut codes: Vec<&'static str> = products
            .iter()
            .filter_map(|p| p.level3_products())
            .flatten()
            .copied()
            .collect();
        codes.sort_unstable();
        codes.dedup();
        codes
    }

    /// Which object of a paired volume this product's Level III rendition is —
    /// what [`crate::level3::product_from_candidates`] is given when a
    /// particular volume's object is wanted (a loop frame, a validation twin).
    ///
    /// [`crate::level3::VolumePick::Latest`] for the QPE family, which emits an
    /// end-of-volume composite *plus* a partial intermediate per SAILS/MRLE
    /// scan under the same volume start: the nearest-to-start candidate there is
    /// an intermediate, and a loop paired that way would animate partial
    /// accumulations. Nearest for everything else, which publishes once per
    /// volume.
    ///
    /// Meaningless for a Level II product, and it says so — `None` rather than a
    /// default nobody should read.
    ///
    /// **Every product naming a given code must answer the same pick.** Objects
    /// are cached per code and shared by every product that reads them (see
    /// [`level3_readers`](Self::level3_readers)), so two products that shared a
    /// code and disagreed here would take turns overwriting one cache entry with
    /// the other's choice of object. Today the only shared codes are `DVL` and
    /// `EET`, all of whose readers are `Nearest`, and
    /// `every_shared_level3_code_agrees_on_its_volume_pick` in
    /// [`crate::level3`] holds that.
    pub fn level3_volume_pick(&self) -> Option<crate::level3::VolumePick> {
        crate::product_spec::spec(*self).level3_volume_pick
    }

    /// A stable identifier for this product on a wire.
    ///
    /// Deliberately not the enum's declaration order and not the serde
    /// representation: reordering or renaming the variants must not silently
    /// change what an already-encoded message means. Both message formats that
    /// cross the browser's worker boundary — [`crate::render_input`]'s payload
    /// and `rustdar_frontend::offload`'s job framing — read this one table.
    ///
    /// The registration in [`crate::product_spec::spec`] is exhaustive with
    /// every field explicit, so a new variant fails to compile until it is
    /// given a code.
    pub fn wire_code(&self) -> u16 {
        crate::product_spec::spec(*self).wire_code
    }

    /// The inverse of [`wire_code`](Self::wire_code). `None` for a code this
    /// build does not know, which is a message from another build rather than a
    /// bug to panic on.
    pub fn from_wire_code(code: u16) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|p| crate::product_spec::spec(*p).wire_code == code)
    }

    /// Which of a radial's moment fields this product reads.
    ///
    /// The single product → moment table. [`get_moment`](Self::get_moment)
    /// reads a radial *through* it rather than repeating it, so a consumer that
    /// needs to name the field — [`crate::render_input`], which has to place a
    /// moment back on a reconstructed radial — cannot come to disagree with the
    /// consumer that reads it.
    pub fn moment_slot(&self) -> Option<MomentSlot> {
        crate::product_spec::spec(*self).moment_slot
    }

    /// The moment data for this product on a radial.
    pub fn get_moment<'a>(&self, radial: &'a Radial) -> Option<&'a nexrad_model::data::MomentData> {
        self.moment_slot()?.read(radial)
    }

    /// Whether this product **is** a moment the RDA put on the wire, rather
    /// than a field computed from one.
    ///
    /// The question a readout has to ask before reading a number out of a
    /// volume. [`Self::moment_slot`] says which moment a product *needs* and is
    /// `Some` for seventeen products; six of them need their own, and the other
    /// eleven need one to compute from. Normalized rotation reads velocity,
    /// storm-relative velocity reads velocity, the hail pair and the hybrid
    /// classification read reflectivity — and handing any of those the raw
    /// moment would print metres per second under a colour scale that means
    /// shear, in a place where nothing would look wrong.
    ///
    /// Written as a round trip through [`MomentSlot::product`] rather than as a
    /// second list of six, so a product added to either table cannot be
    /// classified twice. `every_product_is_its_own_moment_or_is_derived` walks
    /// the whole enum against it.
    pub fn is_wire_moment(&self) -> bool {
        self.moment_slot()
            .is_some_and(|slot| slot.product() == *self)
    }

    /// Whether this product reads every tilt carrying its moment, rather than
    /// the one sweep `crate::render::find_sweep` picks.
    ///
    /// The single product → how-much-of-the-volume table, for the same reason
    /// [`moment_slot`](Self::moment_slot) is the single product → moment one:
    /// three separate paths ask this question and every one of them has to get
    /// the same answer.
    ///
    /// - [`crate::render_input::RenderInput::extract`] reads it to decide how
    ///   many sweeps travel to the renderer.
    /// - `rustdar_frontend`'s `cut_selection_for` reads it to decide how much
    ///   of a live volume the chunk feed downloads *at all*
    ///   ([`crate::chunks::CutSelection`]).
    /// - `rustdar_frontend`'s `reset_panes_for_tilts` reads it to decide whether
    ///   a completed cut re-renders a pane or leaves it for the wider reset a
    ///   closing volume does.
    ///
    /// They each used to carry their own copy of the match. The copy the chunk
    /// feed read omitted [`StormRelativeVelocity`](Self::StormRelativeVelocity),
    /// so a live SRV pane narrowed its site's feed to a single tilt while SRV
    /// went on fitting its dealias seed and its derived storm motion vector
    /// from "every velocity tilt" — of a volume that had deliberately skipped
    /// cuts.
    ///
    /// That is the failure mode of every product below, and it is invisible:
    /// each walks only the tilts *present* — `compute_echo_tops` clamps every
    /// column to the topmost one, a wind profile fits whatever tilts it is
    /// handed — so a partial volume yields a plausible, wrong answer with no
    /// error and no NaN to notice.
    ///
    /// Exhaustive, like [`wire_code`](Self::wire_code): a new variant fails to
    /// compile until it has been classified in its
    /// [`crate::product_spec::spec`] registration.
    pub fn reads_whole_volume(&self) -> bool {
        crate::product_spec::spec(*self).reads_whole_volume
    }

    /// Whether a **plan view** of this product draws the same picture whatever
    /// tilt is selected, so everything that keys a plan-view raster on the
    /// elevation may drop that half of the key.
    ///
    /// Four Level II products qualify, and they are the four
    /// [`crate::render::render_radar_to_image_full`] dispatches *before* it
    /// calls `find_sweep`: interpolated echo tops, the hail pair, and the
    /// hybrid classification. Each reduces the whole volume to one polar grid,
    /// and the `elevation_angle` argument reaches no line of any of them —
    /// `render_echo_tops_interp_to_image` says so in its own doc:
    /// "Tilt-independent — every elevation request renders the same volume
    /// product."
    ///
    /// **Derived from the two exhaustive predicates rather than restated as a
    /// list.** [`crate::derive::volume_slot`] is `None` for exactly the
    /// products with no per-tilt field and no per-tilt derivation, and
    /// [`is_level3`](Self::is_level3) removes the ones whose pixels come from
    /// an RPG object instead of a Level II tilt (those keep the elevation axis:
    /// their objects *are* per-tilt). A hand-kept fifth copy of "which products
    /// read the whole volume" is the mistake
    /// [`reads_whole_volume`](Self::reads_whole_volume) documents having
    /// already been paid for once — a copy that omitted SRV, so live panes
    /// fitted their dealias seed from volumes the feed had skipped cuts of.
    /// `reads_whole_volume` is the wrong predicate to reuse here: it is also
    /// true of NROT and SRV, which rasterize the sweep `find_sweep` picks and
    /// so really do change with the tilt.
    ///
    /// # It lives on the product, in this crate, because two crates ask it
    ///
    /// `rustdar_frontend`'s `render_cache_key` asks it to collapse those four
    /// into one cache slot, and `rustdar_egui`'s
    /// `LoopPlaybackState::retarget_renders_keyed` asks it to keep a plan-view
    /// loop's frames when only the tilt moved. `rustdar_frontend` depends on
    /// `rustdar_egui`, so the second cannot call into the first, and a second
    /// copy of the list in the crate that cannot reach the original is exactly
    /// the failure `reads_whole_volume` above describes. Both depend on this
    /// crate, which is also where the two predicates it is derived from live.
    ///
    /// Without this, a tilt click on one of those four panes missed the cache
    /// and paid a full whole-volume recompute — measured at 6.9 s for a 14-tilt
    /// dual-pol hybrid classification — to redraw a byte-identical picture; and
    /// on a *looping* pane it discarded every frame and paid that per frame.
    pub fn tilt_independent_plan_view(&self) -> bool {
        !self.is_level3() && crate::derive::volume_slot(*self).is_none()
    }

    /// Whether this product's picture is a function of the environmental
    /// 0 °C / −20 °C heights — the per-site pair a sounding lands
    /// ([`crate::sounding`]), which rides the render parameters rather than a
    /// moment because no radial carries it.
    ///
    /// **The one statement of that set.** Three places have to agree about it
    /// and each used to say it for itself: which products carry the pair
    /// across the worker port ([`crate::render_input`]), which are handed it
    /// in their render parameters, and which have to be redrawn when a
    /// sounding moves it. The third copy named the hail pair alone, so an HCA
    /// pane kept a default-melting-layer classification after a sounding
    /// landed and until the volume rolled — a wrong picture, not a stale one,
    /// and the reason this is a method rather than three `matches!`.
    ///
    /// Exhaustive, like [`reads_whole_volume`](Self::reads_whole_volume): a
    /// new variant fails to compile until it has been classified in its
    /// [`crate::product_spec::spec`] registration, which is the only way the
    /// three agree by construction rather than by review.
    pub fn reads_env_heights(&self) -> bool {
        crate::product_spec::spec(*self).reads_env_heights
    }

    /// Format a radar product value for display (e.g. in a hover tooltip).
    ///
    /// Deliberately **not** collapsed into the [`crate::product_spec::spec`]
    /// registration or `Measured::display` at M4: the string shapes vary per
    /// product — decimals of 0, 1, 2 and 4; spaced, glued and absent suffixes
    /// (CC prints no unit while `unit_label` returns `"CC"`; ΦDP glues its
    /// degree sign); HCA prints a categorical class, not a number — and
    /// forcing them through one display shape would change pinned UI strings.
    /// It migrates at E9 with FieldFacts.
    pub fn format_value(&self, value: f32, prefs: &UserPreferences) -> String {
        match self {
            RadarProduct::Reflectivity => format!("Reflectivity: {:.1} dBZ", value),
            RadarProduct::Velocity | RadarProduct::StormRelativeVelocity => {
                let converted = prefs.speed.convert_from_ms(value);
                format!("{}: {:.1} {}", self.name(), converted, prefs.speed.suffix())
            }
            RadarProduct::SpectrumWidth => {
                let converted = prefs.speed.convert_from_ms(value);
                format!("Spectrum Width: {:.1} {}", converted, prefs.speed.suffix())
            }
            RadarProduct::DifferentialReflectivity => {
                format!("Diff. Reflectivity: {:.2} dB", value)
            }
            RadarProduct::CorrelationCoefficient => format!("Corr. Coefficient: {:.4}", value),
            RadarProduct::DifferentialPhase => format!("Diff. Phase: {:.1}°", value),
            RadarProduct::SpecificDifferentialPhase => format!("KDP: {:.2} °/km", value),
            RadarProduct::EchoTops | RadarProduct::EchoTopsInterpolated => {
                let converted = prefs.height.convert_kft_to_kilo(value);
                format!(
                    "{}: {:.1} {}",
                    self.name(),
                    converted,
                    prefs.height.kilo_suffix()
                )
            }
            RadarProduct::VerticallyIntegratedLiquid => format!("VIL: {:.1} kg/m²", value),
            RadarProduct::VilDensity => format!("VIL Density: {:.2} g/m³", value),
            RadarProduct::ProbabilityOfSevereHail => format!("POSH: {:.0}%", value),
            // The field computes in mm (`crate::hail`); the render seam
            // converts to inches, so the value arrives here in inches — the
            // unit US hail sizes are reported in — and the hail-size preference
            // takes it from there, at the precision that unit reads well in
            // (`HailSizeUnit::decimals`). The suffix comes from `unit_label`, so
            // this readout and the colour bar beside it cannot name different
            // units.
            RadarProduct::MaxExpectedHailSize => {
                let converted = prefs.hail_size.convert_from_inches(value);
                let decimals = prefs.hail_size.decimals();
                format!("MEHS: {converted:.decimals$} {}", self.unit_label(prefs))
            }
            RadarProduct::HydrometeorClassification => {
                let class = match value as u16 {
                    0..=9 => "No Data",
                    10..=19 => "Biological",
                    20..=29 => "Clutter/AP",
                    30..=39 => "Ice Crystals",
                    40..=49 => "Dry Snow",
                    50..=59 => "Wet Snow",
                    60..=69 => "Rain",
                    70..=79 => "Heavy Rain",
                    80..=89 => "Big Drops",
                    90..=99 => "Graupel",
                    100..=109 => "Hail+Rain",
                    110..=119 => "Large Hail",
                    120..=139 => "Giant Hail",
                    140..=149 => "Unknown",
                    150.. => "Range Folded",
                };
                format!("HHC: {class}")
            }
            RadarProduct::PrecipitationRate => {
                let converted = prefs.precip_rate.convert_from_in_per_hr(value);
                format!(
                    "Precip Rate: {:.2} {}",
                    converted,
                    prefs.precip_rate.suffix()
                )
            }
            RadarProduct::NormalizedRotation => format!("NROT: {:.2}", value),
        }
    }

    /// Short unit label for this product (used in the color scale legend).
    ///
    /// Derived from the registration's [`Quantity`]: a `Unitless` label
    /// prints as itself, every other quantity takes the preferred unit's
    /// suffix — including MEHS's Inches→`"in"` colour-bar rule, which lives
    /// on [`Quantity::suffix`] now.
    pub fn unit_label(&self, prefs: &UserPreferences) -> &'static str {
        match self.quantity() {
            Quantity::Unitless { label } => label,
            q => q.suffix(prefs),
        }
    }

    /// The unit domain this product's values live in, from the registration.
    pub(crate) fn quantity(&self) -> Quantity {
        crate::product_spec::spec(*self).quantity
    }
}

#[cfg(test)]
mod tests;
