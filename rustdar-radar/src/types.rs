use crate::site_position::{SitePosition, SitePositionSource};
use crate::sites::RadarSite;
use crate::sites::get_radar_site;
use chrono::NaiveDateTime;
use nexrad_model::data::Radial;
use nexrad_model::data::Scan;
use rustdar_units::{HailSizeUnit, UserPreferences};
use std::collections::HashMap;
use std::f64::consts::PI;

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
pub const WASM_IMAGE_SIZE: usize = 1024;

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

/// Side length, in pixels, of the square radar image every render produces.
/// An RGBA texture is `IMAGE_SIZE² × 4` bytes; a static pane render keeps an
/// `f32` value grid alongside it, doubling that.
///
/// wasm32 halves the side: WebGL2 only guarantees
/// `max_texture_dimension_2d == 2048` ([`WEBGL2_MAX_TEXTURE_DIMENSION_2D`]), so
/// a 2048² frame sits exactly on the limit with nothing spare for the overlay
/// textures beside it.
///
/// The two arms select between [`WASM_IMAGE_SIZE`] and [`NATIVE_IMAGE_SIZE`]
/// rather than repeating their literals, so the *selection* is the only thing
/// here a host build cannot check.
#[cfg(target_arch = "wasm32")]
pub const IMAGE_SIZE: usize = WASM_IMAGE_SIZE;
#[cfg(not(target_arch = "wasm32"))]
pub const IMAGE_SIZE: usize = NATIVE_IMAGE_SIZE;

pub const MAX_RANGE_KM: f64 = 230.0; // NEXRAD max range ~230km
pub const PIXELS_PER_KM: f64 = IMAGE_SIZE as f64 / (2.0 * MAX_RANGE_KM);
/// Mean radius of Earth in kilometers — the IUGG mean radius, and the one
/// sphere every *horizontal* measurement in this workspace stands on.
///
/// # This is the horizontal-geodesy radius, not a propagation radius
///
/// Three different quantities in this crate are spelled with a number near
/// 6371 and they are not interchangeable:
///
/// * **This one.** Degrees ↔ kilometres on the ground: where a gate is
///   painted, where the image bounds fall, how far the cursor is from the
///   site, where a cross-section's ground track runs. One sphere, because
///   the *only* thing that matters is that the data and the map under it
///   agree; see [`KM_PER_DEGREE_LAT`].
/// * **[`crate::beam::RE_EFF_KM`]**, `6371 · 4/3`. An atmospheric refraction
///   model that happens to be derived from the same figure. Changing it is a
///   change to beam physics, not to geodesy.
/// * **The `1.21 · 6371` Level III models** in [`crate::eet`],
///   [`crate::dpprep`] and [`crate::hca`]. Each reproduces an RPG product
///   bit-for-bit and each says so at its own constant.
///
/// `rustdar-radar/tests/geodesy_one_definition.rs` is the guard that keeps
/// the first of those three from acquiring a fourth spelling; it carries the
/// reason for every other site in the workspace that names one of these
/// numbers.
pub const EARTH_RADIUS_KM: f64 = 6371.0;

/// Kilometres per degree of latitude on [`EARTH_RADIUS_KM`]: 111.194927 km.
///
/// **Derived, never written down.** This is the workspace's single conversion
/// between angle and ground distance, and it is an expression over
/// [`EARTH_RADIUS_KM`] precisely so that no caller can hold a different
/// planet from the one [`crate::render::render_gate`] paints gates on. The
/// only copy that is not this expression is `volume.wgsl`'s, which cannot
/// see Rust; `rustdar-frontend`'s
/// `the_shaders_km_per_degree_is_the_radar_crates_own` pins that literal to
/// this value.
///
/// # It used to be 111.32, and 111.32 is the equatorial figure
///
/// `ImageBounds::from_radar_site` and everything downstream of it — the
/// plan-view range ring, the volume floor, the region-drag preview — spelled
/// `111.32`, which is a degree on a 6378.1 km (WGS-84 *equatorial*) sphere,
/// while the radar data itself was placed on 6371. The gap is 0.11 %: 0.26 km
/// at the 230 km raster edge, biased one way rather than averaging out, so
/// echoes sat consistently outside the geography drawn under them and the
/// error grew with range.
///
/// Neither figure is "correct" — a real degree of latitude runs 110.57 km at
/// the equator to 111.69 km at the poles — so the choice is consistency, not
/// accuracy. It resolved to 6371 because that is the sphere the *data* is on
/// (`render_gate`, [`crate::beam::site_bearing_range_km`],
/// `great_circle_point`, the voxel builder): framing follows the data rather
/// than the other way round. It is also the better of the two figures for
/// the latitudes this application serves — a degree at 35–45 °N is
/// 110.94–111.13 km, which 111.195 misses by ~0.1 % and 111.32 by ~0.25 %.
pub const KM_PER_DEGREE_LAT: f64 = EARTH_RADIUS_KM * PI / 180.0;

/// m/s to mph conversion factor.
pub const MS_TO_MPH: f32 = 2.23694;

#[inline]
pub(crate) fn lat_rad_to_mercator_y(lat_rad: f64) -> f64 {
    (PI / 4.0 + lat_rad / 2.0).tan().ln()
}

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
    /// Extent is `MAX_RANGE_KM` in every direction from the site.
    ///
    /// On [`KM_PER_DEGREE_LAT`], which is [`EARTH_RADIUS_KM`] — the same
    /// sphere [`crate::render::render_gate`] paints the gates inside these
    /// bounds on. It read `111.32` until the two were unified; see that
    /// constant for what moved.
    pub fn from_radar_site(radar_lat: f64, radar_lon: f64) -> Self {
        let radar_lat_rad = radar_lat.to_radians();
        let lat_deg_per_km = 1.0 / KM_PER_DEGREE_LAT;
        let lon_deg_per_km = 1.0 / (KM_PER_DEGREE_LAT * radar_lat_rad.cos());

        let max_lat_offset = MAX_RANGE_KM * lat_deg_per_km;
        let max_lon_offset = MAX_RANGE_KM * lon_deg_per_km;

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
    /// 2. **A position learned from an earlier volume**, supplied by the
    ///    caller out of its own store. This is what makes a site stay
    ///    corrected across restarts, and what lets the map centre correctly on
    ///    a site opened before but not yet re-downloaded this session.
    ///
    /// 3. **[`crate::sites::radars()`]**, the compiled-in snapshot. Still the
    ///    answer for a pre-2010 `AR2V0001` volume, which is Message 1
    ///    throughout and carries no Volume Data Block to read, and for a
    ///    chunk-fed `Scan`, which is assembled by `crate::chunks` through
    ///    `Scan::new` and has no site on it by construction.
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
        let (site_position, site_source) = match (
            data.site().and_then(SitePosition::from_volume),
            learned,
            row.is_some(),
        ) {
            (Some(volume), _, _) => (Some(volume), SitePositionSource::Volume),
            (None, Some(learned), _) => (Some(learned), SitePositionSource::Learned),
            (None, None, true) => (None, SitePositionSource::Table),
            (None, None, false) => (None, SitePositionSource::Unknown),
        };

        let radar_site = match (site_position, row) {
            (Some(position), row) => position.applied_to(row),
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
                    name: crate::sites::UNKNOWN_SITE_NAME,
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
        match self {
            RadarProduct::Reflectivity => "ref",
            RadarProduct::Velocity => "vel",
            RadarProduct::SpectrumWidth => "sw",
            RadarProduct::DifferentialPhase => "phi",
            RadarProduct::CorrelationCoefficient => "rho",
            RadarProduct::DifferentialReflectivity => "zdr",
            RadarProduct::StormRelativeVelocity => "srv",
            RadarProduct::SpecificDifferentialPhase => "kdp",
            RadarProduct::EchoTops => "eet",
            RadarProduct::EchoTopsInterpolated => "eti",
            RadarProduct::VerticallyIntegratedLiquid => "vil",
            RadarProduct::VilDensity => "vild",
            RadarProduct::ProbabilityOfSevereHail => "posh",
            RadarProduct::MaxExpectedHailSize => "mehs",
            RadarProduct::HydrometeorClassification => "hhc",
            RadarProduct::PrecipitationRate => "dpr",
            RadarProduct::NormalizedRotation => "nrot",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            RadarProduct::Reflectivity => "Reflectivity",
            RadarProduct::Velocity => "Velocity",
            RadarProduct::SpectrumWidth => "Spectrum Width",
            RadarProduct::DifferentialPhase => "Differential Phase",
            RadarProduct::CorrelationCoefficient => "Correlation Coefficient",
            RadarProduct::DifferentialReflectivity => "Differential Reflectivity",
            RadarProduct::StormRelativeVelocity => "Storm-Relative Velocity",
            RadarProduct::SpecificDifferentialPhase => "Specific Differential Phase",
            RadarProduct::EchoTops => "Echo Tops",
            RadarProduct::EchoTopsInterpolated => "Echo Tops (Interp)",
            RadarProduct::VerticallyIntegratedLiquid => "Vertically Integrated Liquid",
            RadarProduct::VilDensity => "VIL Density",
            RadarProduct::ProbabilityOfSevereHail => "Prob. of Severe Hail",
            RadarProduct::MaxExpectedHailSize => "Max Expected Hail Size",
            RadarProduct::HydrometeorClassification => "Hydrometeor Classification",
            RadarProduct::PrecipitationRate => "Precipitation Rate",
            RadarProduct::NormalizedRotation => "Normalized Rotation",
        }
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
        match self {
            RadarProduct::Reflectivity => 0,
            RadarProduct::Velocity => 1,
            RadarProduct::SpectrumWidth => 2,
            RadarProduct::DifferentialReflectivity => 3,
            RadarProduct::CorrelationCoefficient => 4,
            RadarProduct::DifferentialPhase => 5,
            RadarProduct::NormalizedRotation => 6,
            RadarProduct::StormRelativeVelocity => 7,
            RadarProduct::SpecificDifferentialPhase => 8,
            RadarProduct::EchoTops => 9,
            RadarProduct::EchoTopsInterpolated => 10,
            RadarProduct::VerticallyIntegratedLiquid => 11,
            RadarProduct::VilDensity => 12,
            RadarProduct::ProbabilityOfSevereHail => 13,
            RadarProduct::MaxExpectedHailSize => 14,
            RadarProduct::HydrometeorClassification => 15,
            RadarProduct::PrecipitationRate => 16,
        }
    }

    pub fn is_level3(&self) -> bool {
        matches!(
            self,
            RadarProduct::SpecificDifferentialPhase
                | RadarProduct::EchoTops
                | RadarProduct::VerticallyIntegratedLiquid
                | RadarProduct::VilDensity
                | RadarProduct::PrecipitationRate
        )
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
    /// Level II volume already in hand, dealiased locally with a Bunkers
    /// right-mover default vector. See [`crate::srv`].
    pub fn level3_products(&self) -> Option<&'static [&'static str]> {
        match self {
            RadarProduct::SpecificDifferentialPhase => Some(&["N0K"]),
            RadarProduct::EchoTops => Some(&["EET"]),
            RadarProduct::VerticallyIntegratedLiquid => Some(&["DVL"]),
            RadarProduct::VilDensity => Some(&["DVL", "EET"]),
            RadarProduct::PrecipitationRate => Some(&["DPR"]),
            _ => None,
        }
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
        if !self.is_level3() {
            return None;
        }
        Some(match self {
            RadarProduct::PrecipitationRate => crate::level3::VolumePick::Latest,
            _ => crate::level3::VolumePick::NEAREST,
        })
    }

    /// A stable identifier for this product on a wire.
    ///
    /// Deliberately not the enum's declaration order and not the serde
    /// representation: reordering or renaming the variants must not silently
    /// change what an already-encoded message means. Both message formats that
    /// cross the browser's worker boundary — [`crate::render_input`]'s payload
    /// and `rustdar_frontend::offload`'s job framing — read this one table.
    ///
    /// The match is exhaustive, so a new variant fails to compile until it is
    /// given a code.
    pub fn wire_code(&self) -> u16 {
        match self {
            RadarProduct::Reflectivity => 1,
            RadarProduct::Velocity => 2,
            RadarProduct::SpectrumWidth => 3,
            RadarProduct::DifferentialPhase => 4,
            RadarProduct::CorrelationCoefficient => 5,
            RadarProduct::DifferentialReflectivity => 6,
            RadarProduct::StormRelativeVelocity => 7,
            RadarProduct::SpecificDifferentialPhase => 8,
            RadarProduct::EchoTops => 9,
            RadarProduct::EchoTopsInterpolated => 10,
            RadarProduct::VerticallyIntegratedLiquid => 11,
            RadarProduct::HydrometeorClassification => 12,
            RadarProduct::PrecipitationRate => 13,
            RadarProduct::NormalizedRotation => 14,
            RadarProduct::VilDensity => 15,
            RadarProduct::ProbabilityOfSevereHail => 16,
            RadarProduct::MaxExpectedHailSize => 17,
        }
    }

    /// The inverse of [`wire_code`](Self::wire_code). `None` for a code this
    /// build does not know, which is a message from another build rather than a
    /// bug to panic on.
    pub fn from_wire_code(code: u16) -> Option<Self> {
        let product = match code {
            1 => RadarProduct::Reflectivity,
            2 => RadarProduct::Velocity,
            3 => RadarProduct::SpectrumWidth,
            4 => RadarProduct::DifferentialPhase,
            5 => RadarProduct::CorrelationCoefficient,
            6 => RadarProduct::DifferentialReflectivity,
            7 => RadarProduct::StormRelativeVelocity,
            8 => RadarProduct::SpecificDifferentialPhase,
            9 => RadarProduct::EchoTops,
            10 => RadarProduct::EchoTopsInterpolated,
            11 => RadarProduct::VerticallyIntegratedLiquid,
            12 => RadarProduct::HydrometeorClassification,
            13 => RadarProduct::PrecipitationRate,
            14 => RadarProduct::NormalizedRotation,
            15 => RadarProduct::VilDensity,
            16 => RadarProduct::ProbabilityOfSevereHail,
            17 => RadarProduct::MaxExpectedHailSize,
            _ => return None,
        };
        debug_assert_eq!(product.wire_code(), code);
        Some(product)
    }

    /// Which of a radial's moment fields this product reads.
    ///
    /// The single product → moment table. [`get_moment`](Self::get_moment)
    /// reads a radial *through* it rather than repeating it, so a consumer that
    /// needs to name the field — [`crate::render_input`], which has to place a
    /// moment back on a reconstructed radial — cannot come to disagree with the
    /// consumer that reads it.
    pub fn moment_slot(&self) -> Option<MomentSlot> {
        match self {
            RadarProduct::Reflectivity => Some(MomentSlot::Reflectivity),
            RadarProduct::Velocity => Some(MomentSlot::Velocity),
            RadarProduct::SpectrumWidth => Some(MomentSlot::SpectrumWidth),
            RadarProduct::DifferentialReflectivity => Some(MomentSlot::DifferentialReflectivity),
            RadarProduct::CorrelationCoefficient => Some(MomentSlot::CorrelationCoefficient),
            RadarProduct::DifferentialPhase => Some(MomentSlot::DifferentialPhase),
            // NROT is derived from velocity
            RadarProduct::NormalizedRotation => Some(MomentSlot::Velocity),
            // Storm-relative velocity is derived from velocity too — every
            // velocity tilt lists, an upgrade over the four fixed Level III
            // tilts the product used to fetch. See `crate::srv`.
            RadarProduct::StormRelativeVelocity => Some(MomentSlot::Velocity),
            // Interpolated echo tops integrate the whole reflectivity volume;
            // tying availability to the reflectivity moment lists it alongside
            // the reflectivity tilts (the rendered field is tilt-independent).
            RadarProduct::EchoTopsInterpolated => Some(MomentSlot::Reflectivity),
            // The hail pair integrates the whole reflectivity volume too
            // (`crate::hail`); the environmental heights it also needs ride
            // the render parameters, not a moment.
            RadarProduct::ProbabilityOfSevereHail | RadarProduct::MaxExpectedHailSize => {
                Some(MomentSlot::Reflectivity)
            }
            // The hybrid hydrometeor classification composites every dual-pol
            // tilt of the volume (crate::hhc); listing on reflectivity puts
            // the tilt-independent volume product alongside the reflectivity
            // tilts, the same convention as ETI and VIL density. The render
            // payload carries the rest of the moments (crate::render_input's
            // extras).
            RadarProduct::HydrometeorClassification => Some(MomentSlot::Reflectivity),
            // Level III products. No Level II moment stands behind them.
            //
            // VIL density is here rather than on reflectivity: it used to be a
            // local quotient of two whole-volume integrals, and is now the
            // RPG's own `DVL` over its own `EET` ([`crate::vild`]) because the
            // local version was measured mute at the thresholds it is read for
            // (see [`crate::vil`]'s validation section).
            RadarProduct::SpecificDifferentialPhase
            | RadarProduct::EchoTops
            | RadarProduct::VerticallyIntegratedLiquid
            | RadarProduct::VilDensity
            | RadarProduct::PrecipitationRate => None,
        }
    }

    /// The moment data for this product on a radial.
    pub fn get_moment<'a>(&self, radial: &'a Radial) -> Option<&'a nexrad_model::data::MomentData> {
        self.moment_slot()?.read(radial)
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
    /// went on fitting its dealias seed and its default Bunkers vector from
    /// "every velocity tilt" — of a volume that had deliberately skipped cuts.
    ///
    /// That is the failure mode of every product below, and it is invisible:
    /// each walks only the tilts *present* — `compute_echo_tops` clamps every
    /// column to the topmost one, a wind profile fits whatever tilts it is
    /// handed — so a partial volume yields a plausible, wrong answer with no
    /// error and no NaN to notice.
    ///
    /// Exhaustive, like [`wire_code`](Self::wire_code): a new variant fails to
    /// compile until it has been classified here.
    pub fn reads_whole_volume(&self) -> bool {
        match self {
            // `volumetric::compute_echo_tops` integrates the whole
            // reflectivity volume. `VolumeCube::build` dedups same-elevation
            // cuts in encounter order, so the tilts have to arrive in scan
            // order as well as all arrive.
            RadarProduct::EchoTopsInterpolated => true,
            // The SHI column integral reads every reflectivity tilt, over the
            // same local VIL machinery echo tops uses (`crate::hail`).
            RadarProduct::ProbabilityOfSevereHail | RadarProduct::MaxExpectedHailSize => true,
            // The selected sweep is what rasterizes, but `build_wind_profile`
            // fits the dealias-seeding profile from every velocity tilt of the
            // volume — the only wind source since the NVW fetch left
            // (`crate::nrot`).
            //
            // Storm-relative velocity has the same shape and one more reason:
            // the profile is also where its default Bunkers vector comes from
            // (`crate::srv`). A user's override does not shrink this —
            // dealias seeding still wants the profile, or render quality would
            // silently vary with whether a vector was typed in.
            RadarProduct::NormalizedRotation | RadarProduct::StormRelativeVelocity => true,
            // The hybrid classification composites every dual-pol tilt down
            // the hybrid scan, and reads every *moment* of them too
            // (`crate::hhc`).
            RadarProduct::HydrometeorClassification => true,
            // One sweep: the rasterizer touches this product's own moment on
            // the sweep `find_sweep` chose and nothing else in the volume.
            RadarProduct::Reflectivity
            | RadarProduct::Velocity
            | RadarProduct::SpectrumWidth
            | RadarProduct::DifferentialPhase
            | RadarProduct::CorrelationCoefficient
            | RadarProduct::DifferentialReflectivity => false,
            // Level III products read no Level II tilt at all — their pixels
            // come from the RPG's own object, which is what
            // `is_level3` covers. `VilDensity` was in the
            // set above when it was a local quotient of two whole-volume
            // integrals, and left it along with the integrals
            // (`crate::vild`).
            RadarProduct::SpecificDifferentialPhase
            | RadarProduct::EchoTops
            | RadarProduct::VerticallyIntegratedLiquid
            | RadarProduct::VilDensity
            | RadarProduct::PrecipitationRate => false,
        }
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
    /// new variant fails to compile until it has been classified here, which
    /// is the only way the three agree by construction rather than by review.
    pub fn reads_env_heights(&self) -> bool {
        match self {
            // The SHI-to-size mapping has no field at all without the pair:
            // the warning-threshold integral starts at the 0 °C height and
            // is fully weighted above −20 °C, so without them `crate::hail`
            // renders nothing rather than guessing.
            RadarProduct::ProbabilityOfSevereHail | RadarProduct::MaxExpectedHailSize => true,
            // The hybrid classification picks `HsdaHeights::from_env_heights`
            // over `operational_defaults` and feeds both the melting-layer
            // detection, so every class code downstream of the layer moves
            // with the pair (`crate::render::render_hhc_to_image`). Absent a
            // sounding it falls back to the adaptation defaults, exactly as
            // the RPG runs without environmental data — which is why the
            // stale case looked plausible instead of empty.
            RadarProduct::HydrometeorClassification => true,
            // Every other product must never carry the pair, or the byte
            // identity of its payload would depend on an unrelated cache.
            RadarProduct::Reflectivity
            | RadarProduct::Velocity
            | RadarProduct::SpectrumWidth
            | RadarProduct::DifferentialPhase
            | RadarProduct::CorrelationCoefficient
            | RadarProduct::DifferentialReflectivity
            | RadarProduct::StormRelativeVelocity
            | RadarProduct::SpecificDifferentialPhase
            | RadarProduct::EchoTops
            | RadarProduct::EchoTopsInterpolated
            | RadarProduct::VerticallyIntegratedLiquid
            | RadarProduct::VilDensity
            | RadarProduct::PrecipitationRate
            | RadarProduct::NormalizedRotation => false,
        }
    }

    /// Format a radar product value for display (e.g. in a hover tooltip).
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
    pub fn unit_label(&self, prefs: &UserPreferences) -> &'static str {
        match self {
            RadarProduct::Reflectivity => "dBZ",
            RadarProduct::Velocity | RadarProduct::StormRelativeVelocity => prefs.speed.suffix(),
            RadarProduct::SpectrumWidth => prefs.speed.suffix(),
            RadarProduct::DifferentialReflectivity => "dB",
            RadarProduct::CorrelationCoefficient => "CC",
            RadarProduct::DifferentialPhase => "\u{00b0}",
            RadarProduct::SpecificDifferentialPhase => "\u{00b0}/km",
            RadarProduct::EchoTops | RadarProduct::EchoTopsInterpolated => {
                prefs.height.kilo_suffix()
            }
            RadarProduct::VerticallyIntegratedLiquid => "kg/m\u{00b2}",
            RadarProduct::VilDensity => "g/m\u{00b3}",
            RadarProduct::ProbabilityOfSevereHail => "%",
            // `HailSizeUnit::suffix()` is the inch *mark*, which reads well
            // pressed against a bare number (`1.75"`, as the storm-report popup
            // writes it) but not as a colour-bar title, and not after the space
            // this crate's readouts put before their unit. `in` is also what
            // MEHS has printed since it shipped, so the default reading is
            // character for character what it was. Every other unit takes its
            // own suffix.
            RadarProduct::MaxExpectedHailSize => match prefs.hail_size {
                HailSizeUnit::Inches => "in",
                unit => unit.suffix(),
            },
            RadarProduct::HydrometeorClassification => "HHC",
            RadarProduct::PrecipitationRate => prefs.precip_rate.suffix(),
            RadarProduct::NormalizedRotation => "NROT",
        }
    }
}

#[cfg(test)]
mod tests;
