use crate::sites::RadarSite;
use crate::sites::get_radar_site;
use chrono::NaiveDateTime;
use nexrad_model::data::Radial;
use nexrad_model::data::Scan;
use rustdar_units::{HailSizeUnit, UserPreferences};
use std::collections::HashMap;
use std::f64::consts::PI;

/// Side length, in pixels, of the square radar image every render produces.
/// An RGBA texture is `IMAGE_SIZE² × 4` bytes; a static pane render keeps an
/// `f32` value grid alongside it, doubling that.
///
/// wasm32 halves the side: WebGL2 only guarantees
/// `max_texture_dimension_2d == 2048`, so a 2048² frame sits exactly on the
/// limit with nothing spare for the overlay textures beside it.
#[cfg(target_arch = "wasm32")]
pub const IMAGE_SIZE: usize = 1024;
#[cfg(not(target_arch = "wasm32"))]
pub const IMAGE_SIZE: usize = 2048;

pub const MAX_RANGE_KM: f64 = 230.0; // NEXRAD max range ~230km
pub const PIXELS_PER_KM: f64 = IMAGE_SIZE as f64 / (2.0 * MAX_RANGE_KM);
/// Mean radius of Earth in kilometers.
pub const EARTH_RADIUS_KM: f64 = 6371.0;
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
    pub fn from_radar_site(radar_lat: f64, radar_lon: f64) -> Self {
        let radar_lat_rad = radar_lat.to_radians();
        let lat_deg_per_km = 1.0 / 111.32;
        let lon_deg_per_km = 1.0 / (111.32 * radar_lat_rad.cos());

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
    pub site: RadarSite,
    /// From the first radial's collection timestamp, not the request.
    pub timestamp: NaiveDateTime,
    /// Volume Coverage Pattern number (e.g. 212, 215, 35)
    pub vcp_number: u16,
    pub available_products: Vec<RadarProduct>,
    /// Elevation angles per product, sorted ascending.
    pub product_elevations: HashMap<RadarProduct, Vec<f32>>,
    pub status: String,
}

impl ScanInfo {
    /// Level III products are listed with empty elevation vectors, filled in
    /// later as L3 data arrives.
    pub fn from_scan(data: &Scan, site: &str, requested_timestamp: NaiveDateTime) -> Self {
        let vcp_number = data.coverage_pattern_number().number();

        let product_elevations = discover_product_elevations(data);

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

        let radar_site = get_radar_site(site).cloned().unwrap_or_else(|| {
            log::warn!("Unknown radar site '{}', using fallback location", site);
            RadarSite {
                name: "UNKNOWN",
                lat: 0.0,
                lon: 0.0,
                elev: None,
            }
        });

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
fn discover_product_elevations(scan: &Scan) -> HashMap<RadarProduct, Vec<f32>> {
    let mut product_elevations: HashMap<RadarProduct, Vec<f32>> = HashMap::new();

    for (i, sweep) in scan.sweeps().iter().enumerate() {
        if let Some(first_radial) = sweep.radials().first() {
            let raw_angle = crate::volumetric::sweep_elevation_deg(sweep.radials())
                .unwrap_or_else(|| f64::from(first_radial.elevation_angle_degrees()));
            let elev_angle = (raw_angle * 10.0).round() as f32 / 10.0;

            let mut products_found: Vec<&str> = Vec::new();
            for product in RadarProduct::all() {
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

    for l3_product in RadarProduct::all().iter().filter(|p| p.is_level3()) {
        product_elevations.entry(*l3_product).or_default();
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
mod tests {
    use super::*;
    use nexrad_model::data::{PulseWidth, Scan, VolumeCoveragePattern};

    /// A volume with no sweeps — enough to build a `ScanInfo`, and the strongest
    /// form of the case below: no product can be *discovered* from it.
    fn empty_scan() -> Scan {
        Scan::new(
            VolumeCoveragePattern::new(
                212,
                0,
                0.5,
                PulseWidth::Short,
                false,
                0,
                false,
                0,
                false,
                false,
                0,
                false,
                false,
                Vec::new(),
            ),
            Vec::new(),
        )
    }

    /// Every Level III product is in the selector from the moment a volume loads,
    /// with an empty tilt list — not from the moment its fetch lands.
    ///
    /// This is the availability half of "a user cannot tell which datasource a
    /// product comes from". A product that materialises in the picker a second or
    /// two after the scan, and again after every archive poll (which rebuilds
    /// `ScanInfo` from the volume alone), is a product visibly unlike its
    /// neighbours. Listed from the start, the entry is stable and the angle fills
    /// in behind it; `PaneState::get_rendering_params` reads the empty list as
    /// "the selection stands" so a render is dispatched immediately.
    #[test]
    fn every_level3_product_is_listed_from_the_moment_a_volume_loads() {
        let info = ScanInfo::from_scan(
            &empty_scan(),
            "KTLX",
            chrono::NaiveDate::from_ymd_opt(2026, 7, 26)
                .unwrap()
                .and_hms_opt(1, 48, 0)
                .unwrap(),
        );

        for product in RadarProduct::all().iter().filter(|p| p.is_level3()) {
            assert!(
                info.available_products.contains(product),
                "{} is not selectable until its fetch lands",
                product.name(),
            );
            assert_eq!(
                info.product_elevations.get(product).map(Vec::as_slice),
                Some(&[][..]),
                "{} must be listed with an empty tilt list, not absent",
                product.name(),
            );
        }

        // The volume itself carries nothing, so nothing else is listed: the
        // products above are there because they were added, not because an empty
        // scan happens to yield every product.
        assert_eq!(
            info.available_products.len(),
            RadarProduct::all().iter().filter(|p| p.is_level3()).count(),
            "a sweepless volume listed a Level II product: {:?}",
            info.available_products,
        );
    }

    /// A sweep that opens off its own tilt while the antenna settles: the first
    /// thirty radials ramp from `first` to `flown`, the rest sit on `flown`, so
    /// the sweep's median is `flown` and its first radial is not.
    fn settling_sweep(number: u8, first: f32, flown: f32) -> nexrad_model::data::Sweep {
        use nexrad_model::data::{MomentData, Radial, RadialStatus, Sweep};
        const RADIALS: usize = 360;
        const SETTLING: usize = 30;
        let radials = (0..RADIALS)
            .map(|i| {
                let elevation = if i < SETTLING {
                    first + (flown - first) * (i as f32 / SETTLING as f32)
                } else {
                    flown
                };
                Radial::new(
                    0,
                    i as u16,
                    i as f32 * (360.0 / RADIALS as f32),
                    360.0 / RADIALS as f32,
                    RadialStatus::IntermediateRadialData,
                    number,
                    elevation,
                    Some(MomentData::from_fixed_point(
                        600,
                        0,
                        250,
                        8,
                        2.0,
                        66.0,
                        vec![200u8; 600],
                    )),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(number, radials)
    }

    /// The picker's entries name the tilt each sweep **flew**, not the one it
    /// happened to open on.
    ///
    /// These labels are the values `render::find_sweep` is later handed to find
    /// the sweep again, so the two have to be the same quantity. Read off the
    /// first radial, the two cuts here — 0.44° and 0.84° — would both be offered
    /// as "0.7°", collapsing to a single entry that drew one of them and left
    /// the other with no label that could reach it. That is the KDDC VCP 215
    /// case this whole change is about, and no fixed-elevation fixture can
    /// express it: it needs a sweep whose median and first radial disagree.
    #[test]
    fn the_picker_lists_the_tilt_each_sweep_flew() {
        let scan = Scan::new(
            VolumeCoveragePattern::new(
                215,
                0,
                0.5,
                PulseWidth::Short,
                false,
                0,
                false,
                0,
                false,
                false,
                0,
                false,
                false,
                Vec::new(),
            ),
            vec![
                settling_sweep(1, 0.676, 0.44),
                settling_sweep(2, 0.739, 0.84),
            ],
        );
        let info = ScanInfo::from_scan(
            &scan,
            "KDDC",
            chrono::NaiveDate::from_ymd_opt(2026, 7, 26)
                .unwrap()
                .and_hms_opt(1, 48, 0)
                .unwrap(),
        );

        assert_eq!(
            info.product_elevations
                .get(&RadarProduct::Reflectivity)
                .map(Vec::as_slice),
            Some(&[0.4f32, 0.8][..]),
            "the two cuts must be offered as the tilts they flew — off the first \
             radial both round to 0.7 and one of them disappears",
        );

        // And the labels are usable: each reaches its own cut rather than the
        // two of them sharing one sweep.
        for (label, flown) in [(0.4f32, 0.44f64), (0.8, 0.84)] {
            let found = crate::render::find_sweep(&scan, RadarProduct::Reflectivity, label)
                .unwrap_or_else(|| panic!("{label}° is offered and must be reachable"));
            let drawn =
                crate::volumetric::sweep_elevation_deg(found).expect("the sweep has radials");
            assert!(
                (drawn - flown).abs() < 1e-4,
                "{label}° drew the {drawn}° cut, not the {flown}° one it names",
            );
        }
    }

    /// MEHS is a hail *size*, so it reads in the unit the hail-size preference
    /// asks for — and at the precision that unit carries (`25 mm`, not
    /// `25.40 mm`: the field is quantised in quarter inches, so the hundredth
    /// of a millimetre is arithmetic, not measurement).
    ///
    /// Pinned at the two operational stops of the ramp, the NWS severe-hail
    /// criterion (1.00 in) and SPC significant severe (2.00 in), so the numbers
    /// a warning decision is made on are the ones under test. `unit_label` is
    /// asserted alongside `format_value` because it is the same unit printed in
    /// two places — the hover readout and the colour bar's title — and a pane
    /// that disagreed with itself would be worse than one stuck on inches.
    #[test]
    fn mehs_reads_in_the_users_hail_size_unit() {
        let expected = [
            (HailSizeUnit::Inches, "in", "MEHS: 1.00 in", "MEHS: 2.00 in"),
            (
                HailSizeUnit::Centimeters,
                "cm",
                "MEHS: 2.5 cm",
                "MEHS: 5.1 cm",
            ),
            (
                HailSizeUnit::Millimeters,
                "mm",
                "MEHS: 25 mm",
                "MEHS: 51 mm",
            ),
        ];
        for (unit, label, severe, sig_severe) in expected {
            let prefs = UserPreferences {
                hail_size: unit,
                ..UserPreferences::default()
            };
            assert_eq!(
                RadarProduct::MaxExpectedHailSize.unit_label(&prefs),
                label,
                "{unit:?} colour-bar title",
            );
            assert_eq!(
                RadarProduct::MaxExpectedHailSize.format_value(1.0, &prefs),
                severe,
                "{unit:?} at the 1.00 in severe criterion",
            );
            assert_eq!(
                RadarProduct::MaxExpectedHailSize.format_value(2.0, &prefs),
                sig_severe,
                "{unit:?} at the 2.00 in significant-severe threshold",
            );
        }
    }

    /// …and the default reading is what it was before the preference reached
    /// this arm: `MEHS: {:.2} in`, the literal the arm used to be.
    ///
    /// This is the whole no-silent-change claim. Everyone who has never opened
    /// the settings dialog is on `HailSizeUnit::Inches`, so if any row here
    /// moved, consuming the preference would have quietly restated every hail
    /// size in the app.
    #[test]
    fn mehs_in_inches_is_what_it_printed_before_the_preference_existed() {
        let prefs = UserPreferences::default();
        assert_eq!(
            prefs.hail_size,
            HailSizeUnit::Inches,
            "the premise: inches is the default nobody has to choose",
        );
        for value in [0.0f32, 0.25, 0.75, 1.0, 1.375, 2.0, 4.0, 7.125] {
            assert_eq!(
                RadarProduct::MaxExpectedHailSize.format_value(value, &prefs),
                format!("MEHS: {value:.2} in"),
            );
        }
        assert_eq!(RadarProduct::MaxExpectedHailSize.unit_label(&prefs), "in");
    }
}
