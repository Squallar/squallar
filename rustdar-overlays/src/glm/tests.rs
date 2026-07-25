//! End-to-end tests for the GLM parse path against synthetic granules.
//!
//! The fixture is shaped like a real GOES GLM L2 LCFA file: event-level
//! coordinates, times and energies are `_Unsigned` packed shorts with
//! `scale_factor`/`add_offset`; flash-level coordinates are genuine `float`
//! degrees; areas are packed shorts declared in `m2`. Every constant was copied
//! out of live `noaa-goes19`/`noaa-goes18` granules
//! (`GLM-L2-LCFA/2026/205/12/OR_GLM-L2-LCFA_G{19,18}_s20262051200000_*.nc`).

use super::fetch::{claim_warning, normalize_longitude, parse_glm_netcdf};
use super::{GlmDataLevel, GlmSatellite};

/// `scale_factor` on `event_lat`/`event_lon` in both reference granules.
const COORD_SCALE: f32 = 0.00203128;
/// `add_offset` on `event_lat`.
const LAT_OFFSET: f32 = -66.56;
/// `add_offset` on `event_lon` for the **GOES-East** slot (G16 and G19 alike:
/// it follows the orbital position, not the spacecraft).
const LON_OFFSET_EAST: f32 = -141.56;
/// `add_offset` on `event_lon` for **GOES-West** (G18). 62° further west, so
/// the unpackable interval runs past the antimeridian.
const LON_OFFSET_WEST: f32 = -203.56;
/// `scale_factor`/`add_offset` on every `*_time_offset` variable.
const TIME_SCALE: f32 = 0.0003814756;
const TIME_OFFSET: f32 = -5.0;
/// `scale_factor` on `flash_area`/`group_area`, in m² per pixel count.
const AREA_SCALE: f32 = 152_601.9;
/// `scale_factor`/`add_offset` on `flash_energy`, in joules.
const FLASH_ENERGY_SCALE: f32 = 9.99996e-16;
const ENERGY_OFFSET: f32 = 2.8515e-16;
/// `scale_factor` on `event_energy`.
const EVENT_ENERGY_SCALE: f32 = 1.9024e-17;

const COVERAGE_START: &str = "2026-07-24T12:00:00.0Z";
const TIME_UNITS: &str = "seconds since 2026-07-24 12:00:00.000";

/// Knobs for building granule variants. The defaults reproduce a GOES-East
/// file; individual tests change one thing at a time.
struct GranuleSpec {
    lat_offset: f32,
    lon_offset: f32,
    /// `i16` bit patterns as stored on disk — several are negative and must
    /// come back as large `u16` values.
    event_lat_raw: [i16; 3],
    event_lon_raw: [i16; 3],
    /// `None` omits the attribute entirely.
    area_units: Option<&'static str>,
    energy_units: Option<&'static str>,
    time_units: Option<&'static str>,
    time_coverage_start: &'static str,
}

impl Default for GranuleSpec {
    fn default() -> Self {
        Self {
            lat_offset: LAT_OFFSET,
            lon_offset: LON_OFFSET_EAST,
            // u16 51951 / 51990 / 65535(fill).
            event_lat_raw: [-13585, -13546, -1],
            // u16 36953 / 36959 / 36959.
            event_lon_raw: [-28583, -28577, -28577],
            area_units: Some("m2"),
            energy_units: Some("J"),
            time_units: Some(TIME_UNITS),
            time_coverage_start: COVERAGE_START,
        }
    }
}

/// Start a GLM-shaped granule.
///
/// No `add_dimension` step: NetCDF dimensions are a layer above HDF5, so a
/// dataset's length here *is* its data length.
fn new_granule(time_coverage_start: &str) -> hdf5_pure::FileBuilder {
    let mut w = hdf5_pure::FileBuilder::new();
    w.set_attr(
        "time_coverage_start",
        hdf5_pure::AttrValue::String(time_coverage_start.into()),
    );
    w
}

fn synthetic_granule() -> Vec<u8> {
    granule(&GranuleSpec::default())
}

/// Build a GLM-shaped granule in memory.
fn granule(spec: &GranuleSpec) -> Vec<u8> {
    let mut file = new_granule(spec.time_coverage_start);

    {
        // --- Event level: everything packed, as in the real product. -------

        add_short(
            &mut file,
            "event_lat",
            &spec.event_lat_raw,
            Packed {
                scale: Some(COORD_SCALE),
                offset: Some(spec.lat_offset),
                fill: Some(-1),
                units: Some("degrees_north"),
            },
        );
        add_short(
            &mut file,
            "event_lon",
            &spec.event_lon_raw,
            Packed {
                scale: Some(COORD_SCALE),
                offset: Some(spec.lon_offset),
                fill: None,
                units: Some("degrees_east"),
            },
        );
        // u16 11048 / 39344 / 11048. The middle one is -26192 as i16.
        add_short(
            &mut file,
            "event_time_offset",
            &[11048, -26192, 11048],
            Packed {
                scale: Some(TIME_SCALE),
                offset: Some(TIME_OFFSET),
                fill: None,
                units: spec.time_units,
            },
        );
        // The second event's energy is fill: the strike is still located, so
        // the record survives with an unknown energy.
        add_short(
            &mut file,
            "event_energy",
            &[79, -1, 316],
            Packed {
                scale: Some(EVENT_ENERGY_SCALE),
                offset: Some(ENERGY_OFFSET),
                fill: Some(-1),
                units: spec.energy_units,
            },
        );

        // --- Flash level: float coordinates, packed area/energy/time. ------

        add_float(
            &mut file,
            "flash_lat",
            &[39.033424, -22.65055],
            "degrees_north",
        );
        add_float(
            &mut file,
            "flash_lon",
            &[-66.48116, -52.769894],
            "degrees_east",
        );
        // u16 1826 / 40000; 40000 is -25536 as i16.
        add_short(
            &mut file,
            "flash_area",
            &[1826, -25536],
            Packed {
                scale: Some(AREA_SCALE),
                offset: Some(0.0),
                fill: Some(-1),
                units: spec.area_units,
            },
        );
        add_short(
            &mut file,
            "flash_energy",
            &[75, 8],
            Packed {
                scale: Some(FLASH_ENERGY_SCALE),
                offset: Some(ENERGY_OFFSET),
                fill: Some(-1),
                units: spec.energy_units,
            },
        );
        add_short(
            &mut file,
            "flash_time_offset_of_first_event",
            &[11048, 12461],
            Packed {
                scale: Some(TIME_SCALE),
                offset: Some(TIME_OFFSET),
                fill: None,
                units: spec.time_units,
            },
        );

        // Deliberately no `event_area`: the L2 LCFA product has none.
    }

    file.finish().expect("write granule")
}

struct Packed {
    scale: Option<f32>,
    offset: Option<f32>,
    fill: Option<i16>,
    units: Option<&'static str>,
}

/// A packed `_Unsigned` short, the shape GLM uses for nearly everything.
///
/// `scale_factor`/`add_offset` arrive as `f32` — GLM's declared width — and are
/// widened before writing, so the file holds exactly what a `float` attribute
/// decodes to. An `f64` literal would be a subtly different constant.
fn add_short(file: &mut hdf5_pure::FileBuilder, name: &str, values: &[i16], packed: Packed) {
    let b = file.create_dataset(name);
    b.with_i16_data(values);
    b.set_attr("_Unsigned", hdf5_pure::AttrValue::String("true".into()));
    if let Some(f) = packed.fill {
        b.set_attr("_FillValue", hdf5_pure::AttrValue::I64(i64::from(f)));
    }
    if let Some(s) = packed.scale {
        b.set_attr("scale_factor", hdf5_pure::AttrValue::F64(f64::from(s)));
    }
    if let Some(o) = packed.offset {
        b.set_attr("add_offset", hdf5_pure::AttrValue::F64(f64::from(o)));
    }
    if let Some(u) = packed.units {
        b.set_attr("units", hdf5_pure::AttrValue::String(u.into()));
    }
}

/// An unpacked `float` variable — GLM's `flash_lat`/`group_lat` shape.
fn add_float(file: &mut hdf5_pure::FileBuilder, name: &str, values: &[f32], units: &str) {
    let b = file.create_dataset(name);
    b.with_f32_data(values);
    b.set_attr("units", hdf5_pure::AttrValue::String(units.into()));
}

fn epoch_at(hour: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
        .unwrap()
        .and_hms_opt(hour, 0, 0)
        .unwrap()
}

fn epoch() -> chrono::NaiveDateTime {
    epoch_at(12)
}

fn events_of(bytes: &[u8]) -> Vec<super::GlmFlash> {
    parse_glm_netcdf(bytes, GlmSatellite::GoesEast, &[GlmDataLevel::Event])
        .expect("parse events")
        .records
}

fn flashes_of(bytes: &[u8]) -> Vec<super::GlmFlash> {
    parse_glm_netcdf(bytes, GlmSatellite::GoesWest, &[GlmDataLevel::Flash])
        .expect("parse flashes")
        .records
}

// =====================================================================
// Unpacking
// =====================================================================

/// Fails if CF unpacking is skipped: `event_lat` then reads back as `-13585.0`
/// and every event-level strike is drawn at a garbage coordinate.
#[test]
fn event_level_coordinates_unpack_to_real_positions() {
    let events = events_of(&synthetic_granule());

    // Three events in the file; the third has a `_FillValue` latitude and is
    // unplottable, so it must not appear at all.
    assert_eq!(events.len(), 2, "the fill-latitude event must be dropped");

    assert!((events[0].lat - 38.9670).abs() < 1e-3, "lat was {}", events[0].lat);
    assert!((events[0].lon - (-66.4981)).abs() < 1e-3, "lon was {}", events[0].lon);
    assert!(events.iter().all(|e| (-90.0..=90.0).contains(&e.lat)));
    assert!(events.iter().all(|e| (-180.0..=180.0).contains(&e.lon)));
}

/// `event_time_offset` raw 11048 is `4.2145 s` before `add_offset`; the CF
/// value is `4.2145 - 5.0 = -0.7855 s` from `time_coverage_start`.
#[test]
fn event_times_unpack_against_the_declared_epoch() {
    let events = events_of(&synthetic_granule());

    let first_ms = (events[0].time - epoch()).num_milliseconds();
    assert_eq!(first_ms, -785, "raw 11048 must unpack to -0.785 s");

    // The second event's raw count is 39344, which reads as -26192 from a
    // signed `short`. Unpacked it is 15.0079 - 5 = 10.0079 s.
    let second_ms = (events[1].time - epoch()).num_milliseconds();
    assert_eq!(second_ms, 10_008, "raw 39344 must unpack to +10.008 s");

    // Sanity: both land inside the 20-second granule the file describes,
    // which the unscaled reading (11048 s ≈ 3 hours) never would.
    assert!(events.iter().all(|e| {
        let s = (e.time - epoch()).num_milliseconds();
        (-5_000..25_000).contains(&s)
    }));
}

/// When the time axis's own `units` epoch disagrees with the granule-level
/// `time_coverage_start`, the variable wins. Fails if the per-variable epoch is
/// ignored — which the default fixture cannot catch, the two agreeing there.
#[test]
fn per_variable_epoch_wins_over_time_coverage_start() {
    let bytes = granule(&GranuleSpec {
        // One hour later than `time_coverage_start`.
        time_units: Some("seconds since 2026-07-24 13:00:00.000"),
        ..Default::default()
    });
    let events = events_of(&bytes);

    assert_eq!((events[0].time - epoch_at(13)).num_milliseconds(), -785);
    assert_eq!(
        (events[0].time - epoch_at(12)).num_milliseconds(),
        3_599_214,
        "using time_coverage_start would place this an hour early"
    );
}

/// With no `units` on the time axis, fall back to the global
/// `time_coverage_start` rather than failing.
#[test]
fn absent_time_units_fall_back_to_time_coverage_start() {
    let bytes = granule(&GranuleSpec { time_units: None, ..Default::default() });
    let events = events_of(&bytes);
    assert_eq!((events[0].time - epoch()).num_milliseconds(), -785);
}

/// A `units` string that is *present but uninterpretable* must not fall back
/// like an absent one: that would invent an epoch. `cf` pins that the string
/// does not parse; this pins what `parse_level_records` does about it.
#[test]
fn uninterpretable_time_units_fail_rather_than_guessing() {
    let bytes = granule(&GranuleSpec {
        time_units: Some("fortnights since 2026-07-24 12:00:00.000"),
        ..Default::default()
    });
    let err = parse_glm_netcdf(&bytes, GlmSatellite::GoesEast, &[GlmDataLevel::Event])
        .expect_err("an unreadable time axis must not be guessed at");
    assert!(err.contains("time units"), "the error must say why: {err}");
}

/// The only fixture not on a `"seconds since"` axis, so it is the one that
/// keeps `* seconds_per_unit` from being deletable with the suite green.
#[test]
fn a_millisecond_time_axis_is_scaled_not_misread() {
    let bytes = granule(&GranuleSpec {
        time_units: Some("milliseconds since 2026-07-24 12:00:00.000"),
        ..Default::default()
    });
    let events = events_of(&bytes);

    // Raw 11048 unpacks to -0.785458 *milliseconds*, so the strike sits well
    // under a millisecond before the epoch — not 785 ms before it.
    let us = (events[0].time - epoch()).num_microseconds().expect("in range");
    assert_eq!(us, -785, "expected -0.785 ms, got {us} µs");

    // And the second event, at 10.0088 ms rather than 10.0088 s.
    let us2 = (events[1].time - epoch()).num_microseconds().expect("in range");
    assert_eq!(us2, 10_008, "expected +10.008 ms, got {us2} µs");
}

/// Flash-level `lat`/`lon` are real `float` degrees with no packing
/// attributes, and must survive untouched.
#[test]
fn flash_level_float_coordinates_pass_through_unchanged() {
    let flashes = flashes_of(&synthetic_granule());

    assert_eq!(flashes.len(), 2);
    assert_eq!(flashes[0].lat, f64::from(39.033424_f32));
    assert_eq!(flashes[0].lon, f64::from(-66.48116_f32));
    assert_eq!(flashes[1].lat, f64::from(-22.65055_f32));
}

// =====================================================================
// GOES-West longitude wrapping
// =====================================================================

#[test]
fn longitudes_past_the_antimeridian_wrap_rather_than_vanish() {
    // The GOES-West unpackable floor.
    assert!((normalize_longitude(-203.56) - 156.44).abs() < 1e-9);
    // A real detection measured in a live G18 granule.
    assert!((normalize_longitude(-187.2752) - 172.7248).abs() < 1e-9);
    // Everything already conventional is untouched.
    assert_eq!(normalize_longitude(-141.56), -141.56);
    assert_eq!(normalize_longitude(0.0), 0.0);
    assert_eq!(normalize_longitude(180.0), 180.0);
    assert_eq!(normalize_longitude(-180.0), -180.0);
    // Just past the boundary, on both sides.
    assert!((normalize_longitude(-180.000001) - 179.999999).abs() < 1e-9);
    assert!((normalize_longitude(180.000001) - (-179.999999)).abs() < 1e-9);

    // The ±540 cutoff, pinned from both sides. Beyond it, values are left alone
    // so the downstream range check still sees them.
    //
    // The window is generous: `add_offset` is `sub_point - 66.56` and a
    // sub-point is within ±180, so any geostationary slot's interval lies
    // inside ±246.56 and one wrap always suffices.
    assert_eq!(normalize_longitude(-450.0), -90.0, "inside the window: wraps");
    assert_eq!(normalize_longitude(-540.0), -180.0, "the boundary itself wraps");
    assert_eq!(normalize_longitude(-540.001), -540.001, "just outside: left alone");
    assert_eq!(normalize_longitude(540.0), 180.0);
    assert_eq!(normalize_longitude(540.001), 540.001);
    assert_eq!(normalize_longitude(-1000.0), -1000.0);
}

/// Normalization applies to longitude only. `event_lat:add_offset` is -66.56 on
/// both slots and both epochs sampled, so the unpackable interval is ±66.56 and
/// no wrap is representable — an out-of-range latitude is always a fault.
#[test]
fn latitude_is_never_wrapped() {
    // ~305°, so a ±360 wrap would land at -54.5°, inside the valid interval.
    // Anything below ±270 makes this vacuous: it would be dropped whether or
    // not latitude were normalized.
    let bytes = granule(&GranuleSpec { lat_offset: 200.0, ..Default::default() });
    assert!(
        events_of(&bytes).is_empty(),
        "an out-of-range latitude must be dropped, not folded into a plausible one"
    );
}

/// GOES-West stores longitude in a frame running past the antimeridian, so a
/// real strike at 172.72°E is on disk as -187.28. Dropping those as out of
/// range deleted 60 of 3228 events in the granule this was measured on, and
/// *selectively* — `group_lon`/`flash_lon` are already-wrapped floats and
/// survived, leaving groups on the map with their own events removed.
///
/// The coordinate guard must not fire here.
#[test]
fn goes_west_events_past_the_antimeridian_are_kept() {
    let bytes = granule(&GranuleSpec {
        lon_offset: LON_OFFSET_WEST,
        // u16 8015 unpacks to -187.2793; u16 40000 to -122.3088.
        event_lon_raw: [8015, -25536, 0],
        ..Default::default()
    });
    let events = events_of(&bytes);

    assert_eq!(events.len(), 2, "no GOES-West event may be dropped for being west of -180");
    assert!(
        (events[0].lon - 172.7207).abs() < 1e-3,
        "expected the wrapped eastern-hemisphere longitude, got {}",
        events[0].lon
    );
    assert!(
        (events[1].lon - (-122.3088)).abs() < 1e-3,
        "an in-range GOES-West longitude must be untouched, got {}",
        events[1].lon
    );
    assert!(events.iter().all(|e| (-180.0..=180.0).contains(&e.lon)));
}

/// ...and the other side of the same guard: a coordinate that unpacked to
/// nonsense must not reach the map.
#[test]
fn coordinates_that_are_not_on_the_globe_are_dropped() {
    let bytes = granule(&GranuleSpec {
        // With a zero offset the packed counts unpack to ~105°, off the globe.
        lat_offset: 0.0,
        ..Default::default()
    });
    let events = events_of(&bytes);
    assert!(
        events.is_empty(),
        "latitudes above 90° must be dropped, got {:?}",
        events.iter().map(|e| e.lat).collect::<Vec<_>>()
    );
}

// =====================================================================
// Missing data
// =====================================================================

/// A `_FillValue` on a descriptive field leaves the record, with the field
/// reported as unknown. Zero is not available as a sentinel: every GLM energy
/// variable's `add_offset` alone is 2.85e-16, so zero is out of band, and
/// `rasterize` would draw it as the smallest real bolt.
#[test]
fn fill_valued_energy_becomes_unknown_without_dropping_the_record() {
    let events = events_of(&synthetic_granule());

    // raw 79 → 79 * 1.9024e-17 + 2.8515e-16 = 1.788e-15 J
    let first = events[0].energy.expect("first event has a real energy");
    assert!((f64::from(first) - 1.788e-15).abs() < 1e-17, "energy was {first:e}");
    assert_eq!(events[1].energy, None, "a fill energy must not become a number");
}

/// The L2 LCFA product has no `event_area` variable at all — a property of the
/// product, not a corrupt granule. Events still parse, reporting no area rather
/// than "0.0 km²".
#[test]
fn absent_event_area_variable_reports_unknown_not_zero() {
    let events = events_of(&synthetic_granule());
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e.area.is_none()));
}

/// `flash_area` is a packed count declared in `m2` and the popup labels km², so
/// the parse must both unpack *and* convert. Fails if the raw count (1826)
/// reaches the km² label — wrong by a factor of ~6.6.
#[test]
fn flash_area_unpacks_from_m2_counts_into_km2() {
    let flashes = flashes_of(&synthetic_granule());

    // 1826 * 152601.9 m² = 2.7865e8 m² = 278.65 km²
    let first = flashes[0].area.expect("flash area is reported");
    assert!((first - 278.65).abs() < 0.1, "area was {first}");
    // 40000 stored as -25536; unpacking the signed reading would give a
    // negative area.
    let second = flashes[1].area.expect("flash area is reported");
    assert!((second - 6104.08).abs() < 1.0, "area was {second}");
    // Explicitly not the raw count.
    assert!((first - 1826.0).abs() > 1000.0);
}

// =====================================================================
// Unit policy
// =====================================================================

/// A `units` string we cannot convert makes that *field* unknown; fails if the
/// value is used anyway as if it were canonical.
#[test]
fn unconvertible_area_units_make_the_field_unknown() {
    let bytes = granule(&GranuleSpec {
        area_units: Some("furlongs"),
        ..Default::default()
    });
    let flashes = flashes_of(&bytes);

    assert_eq!(flashes.len(), 2, "unit trouble must not cost us the records");
    assert!(
        flashes.iter().all(|f| f.area.is_none()),
        "an unconvertible unit must not be reported as km²"
    );
}

/// An *absent* `units` attribute is treated exactly like an unconvertible one.
/// Assuming an unlabelled value is already km² reports `flash_area` a million
/// times too large, silently.
#[test]
fn absent_area_units_are_treated_the_same_as_unconvertible_ones() {
    let bytes = granule(&GranuleSpec { area_units: None, ..Default::default() });
    let flashes = flashes_of(&bytes);

    assert_eq!(flashes.len(), 2);
    assert!(
        flashes.iter().all(|f| f.area.is_none()),
        "without a declared unit the value must not be assumed canonical"
    );
}

/// The unit contract applies to energy as to area; without this the check could
/// be deleted from `*_energy` with the suite green. If NOAA re-declared
/// `units = "fJ"`, femtojoules would publish as joules — off by 1e15, putting
/// every bolt at the size ceiling.
#[test]
fn unconvertible_energy_units_make_the_field_unknown() {
    let bytes = granule(&GranuleSpec {
        energy_units: Some("furlongs"),
        ..Default::default()
    });
    let flashes = flashes_of(&bytes);

    assert_eq!(flashes.len(), 2, "unit trouble must not cost us the records");
    assert!(
        flashes.iter().all(|f| f.energy.is_none()),
        "an unconvertible unit must not be reported as joules"
    );
    // Area declares "m2" and is unaffected — the failure is per field.
    assert!(flashes.iter().all(|f| f.area.is_some()));
}

/// Absent energy units are treated the same as unconvertible ones.
#[test]
fn absent_energy_units_are_treated_the_same_as_unconvertible_ones() {
    let bytes = granule(&GranuleSpec { energy_units: None, ..Default::default() });
    let flashes = flashes_of(&bytes);
    assert_eq!(flashes.len(), 2);
    assert!(flashes.iter().all(|f| f.energy.is_none()));
}

/// Unit trouble on one field must not damage the others, nor position or time.
/// Fails if a `flash_area` schema change blacks out the whole granule.
#[test]
fn unit_trouble_is_scoped_to_the_field_it_describes() {
    let bytes = granule(&GranuleSpec {
        area_units: Some("furlongs"),
        ..Default::default()
    });
    let flashes = flashes_of(&bytes);

    // Position and time are untouched.
    assert_eq!(flashes[0].lat, f64::from(39.033424_f32));
    assert_eq!((flashes[0].time - epoch()).num_milliseconds(), -785);
    // Energy still declares "J" and still converts.
    assert!(flashes[0].energy.is_some(), "energy must survive an area unit problem");

    // ...and the other levels parse normally from the same granule.
    let events = events_of(&bytes);
    assert_eq!(events.len(), 2);
}

/// Schema-change warnings fire once and then stay quiet.
#[test]
fn the_warning_registry_reports_each_key_once() {
    let key = format!("registry-probe:{:?}", std::thread::current().id());
    assert!(claim_warning(key.clone()), "first sighting must report");
    assert!(!claim_warning(key.clone()), "a repeat must stay quiet");
    assert!(!claim_warning(key), "and keep staying quiet");
}

/// Pinning the key builders leaves every call site free to inline a different
/// key, so pin the call sites too.
#[test]
fn call_sites_use_the_documented_warning_keys() {
    use super::fetch::{level_parse_key, missing_variable_key, units_key};
    use GlmSatellite::{GoesEast, GoesWest};

    // Asserted against the *registry*, not the log: `claim_warning` returns
    // false for a key already taken, which is deterministic under parallelism
    // in a way that counting log lines is not.

    // --- level_parse_key: satellite- and level-qualified ------------------
    //
    // Group-on-West is a combination no other test breaks, so the claims below
    // can only have come from this parse.
    let broken_group = granule(&GranuleSpec {
        time_units: Some("fortnights since 2026-07-24 12:00:00.000"),
        ..Default::default()
    });
    let _ = parse_glm_netcdf(&broken_group, GoesWest, &[GlmDataLevel::Group]);

    assert!(
        !claim_warning(level_parse_key(GoesWest, "group_lat")),
        "the level-parse call site must key on the satellite and the level"
    );
    assert!(
        claim_warning(level_parse_key(GoesEast, "group_lat")),
        "a West failure must not suppress the report for East"
    );
    assert!(
        claim_warning(level_parse_key(GoesWest, "flash_lat")),
        "a broken Group layer must not suppress the report for Flashes"
    );

    // --- units_key: satellite- and spelling-qualified ---------------------
    //
    // "smoots" is used by no other test, so both satellites' keys start virgin.
    let odd_units = granule(&GranuleSpec {
        area_units: Some("smoots"),
        ..Default::default()
    });
    let _ = flashes_of(&odd_units); // parses as GOES-West

    assert!(
        !claim_warning(units_key(GoesWest, "flash_area", "smoots")),
        "the units call site must key on the satellite, variable and spelling"
    );
    assert!(
        claim_warning(units_key(GoesEast, "flash_area", "smoots")),
        "the same bad unit on the other bird must still be reported"
    );
    assert!(
        claim_warning(units_key(GoesWest, "flash_energy", "smoots")),
        "an area unit problem must not suppress an energy one"
    );

    // --- units_key: the *absent* branch keys the same way ------------------
    //
    // A separate call site from the unrecognized-spelling branch above.
    // `flash_energy` + absent on West is produced by no other test.
    let no_energy_units = granule(&GranuleSpec {
        energy_units: None,
        ..Default::default()
    });
    let _ = flashes_of(&no_energy_units); // parses as GOES-West

    assert!(
        !claim_warning(units_key(GoesWest, "flash_energy", "absent")),
        "the absent-units call site must key on the satellite and variable"
    );
    assert!(
        claim_warning(units_key(GoesEast, "flash_energy", "absent")),
        "a dropped unit on one bird must not suppress the report for the other"
    );

    // --- missing_variable_key: deliberately NOT satellite-qualified -------
    //
    // If the call site added a satellite, the unqualified key below would never
    // be claimed by anyone and this assertion would fail.
    let bytes = {
        let mut file = new_granule(COVERAGE_START);
        add_float(&mut file, "group_lon", &[-97.0], "degrees_east");
        // No `group_lat`.
        file.finish().expect("write")
    };
    let _ = parse_glm_netcdf(&bytes, GoesEast, &[GlmDataLevel::Group]);

    assert!(
        !claim_warning(missing_variable_key("group_lat")),
        "the absent-variable call site must use the unqualified, product-wide key"
    );
}

/// The dedup keys decide which conditions can crowd each other out, so their
/// shape is behaviour, not formatting.
///
/// Asserted on the pure builders rather than by counting log lines: the
/// registry is process-global and other tests parse granules in parallel.
#[test]
fn warning_keys_separate_the_conditions_that_must_not_mask_each_other() {
    use super::fetch::{level_parse_key, missing_variable_key, units_key};
    use GlmSatellite::{GoesEast, GoesWest};

    // The two birds are separate product streams: the same fault on each must
    // be reported twice, not once.
    assert_ne!(
        level_parse_key(GoesEast, "event_lat"),
        level_parse_key(GoesWest, "event_lat"),
        "a level failure must be reported per satellite"
    );
    assert_ne!(
        units_key(GoesEast, "flash_area", "furlongs"),
        units_key(GoesWest, "flash_area", "furlongs"),
        "a unit problem must be reported per satellite"
    );

    // Different levels, variables and spellings are different conditions.
    assert_ne!(
        level_parse_key(GoesEast, "event_lat"),
        level_parse_key(GoesEast, "flash_lat"),
        "one broken level must not mask another"
    );
    assert_ne!(
        units_key(GoesEast, "flash_area", "furlongs"),
        units_key(GoesEast, "flash_energy", "furlongs"),
        "an area unit problem must not mask an energy one"
    );
    assert_ne!(
        units_key(GoesEast, "flash_area", "furlongs"),
        units_key(GoesEast, "flash_area", "cubits"),
        "a second bad spelling is a new condition and must be reported"
    );
    assert_ne!(
        missing_variable_key("event_energy"),
        missing_variable_key("flash_energy"),
        "one absent variable must not mask another"
    );

    // ...but an absent variable is a property of the product schema, so it is
    // reported once across both birds.
    assert_eq!(
        missing_variable_key("event_energy"),
        missing_variable_key("event_energy"),
    );

    // The four namespaces cannot collide with each other.
    let keys = [
        level_parse_key(GoesEast, "event_lat"),
        units_key(GoesEast, "event_lat", "absent"),
        missing_variable_key("event_lat"),
    ];
    for (i, a) in keys.iter().enumerate() {
        for b in &keys[i + 1..] {
            assert_ne!(a, b, "namespaces must not collide");
        }
    }
}

// =====================================================================
// Structural failures
// =====================================================================

/// Every variable at a level shares one dimension, so a short column means a
/// corrupt or restructured file: pairing index `i` across variables would
/// fabricate positions.
#[test]
fn mismatched_column_lengths_are_rejected() {
    let mut file = new_granule(COVERAGE_START);
    {
        let packed = || Packed {
            scale: Some(COORD_SCALE),
            offset: Some(LAT_OFFSET),
            fill: None,
            units: Some("degrees_north"),
        };
        add_short(&mut file, "event_lat", &[-13585, -13546, 11048], packed());
        // One element short: the file is internally inconsistent.
        add_short(&mut file, "event_lon", &[-28583, -28577], packed());
        add_short(
            &mut file,
            "event_time_offset",
            &[11048, 11048, 11048],
            Packed {
                scale: Some(TIME_SCALE),
                offset: Some(TIME_OFFSET),
                fill: None,
                units: Some(TIME_UNITS),
            },
        );
        add_short(
            &mut file,
            "event_energy",
            &[79, 316, 79],
            Packed {
                scale: Some(EVENT_ENERGY_SCALE),
                offset: Some(ENERGY_OFFSET),
                fill: Some(-1),
                units: Some("J"),
            },
        );
    }
    let bytes = file.finish().expect("write");

    let err = parse_glm_netcdf(&bytes, GlmSatellite::GoesEast, &[GlmDataLevel::Event])
        .expect_err("a ragged granule must not parse");
    assert!(
        err.contains("length mismatch") && err.contains("event_lon"),
        "the error must name the offending variable: {err}"
    );
}

/// A level that cannot be parsed must not take the other levels with it: the
/// user selects the three independently.
#[test]
fn one_broken_level_does_not_black_out_the_others() {
    let mut file = new_granule(COVERAGE_START);
    {

        add_short(
            &mut file,
            "event_lat",
            &[-13585, -13546],
            Packed {
                scale: Some(COORD_SCALE),
                offset: Some(LAT_OFFSET),
                fill: None,
                units: Some("degrees_north"),
            },
        );
        add_short(
            &mut file,
            "event_lon",
            &[-28583, -28577],
            Packed {
                scale: Some(COORD_SCALE),
                offset: Some(LON_OFFSET_EAST),
                fill: None,
                units: Some("degrees_east"),
            },
        );
        add_short(
            &mut file,
            "event_time_offset",
            &[11048, 11048],
            Packed {
                scale: Some(TIME_SCALE),
                offset: Some(TIME_OFFSET),
                fill: None,
                units: Some(TIME_UNITS),
            },
        );

        add_short(
            &mut file,
            "event_energy",
            &[79, 316],
            Packed {
                scale: Some(EVENT_ENERGY_SCALE),
                offset: Some(ENERGY_OFFSET),
                fill: Some(-1),
                units: Some("J"),
            },
        );

        // The flash level is internally ragged and cannot parse.
        add_float(&mut file, "flash_lat", &[39.0, 40.0], "degrees_north");
        add_float(&mut file, "flash_lon", &[-97.0], "degrees_east");
        add_short(
            &mut file,
            "flash_time_offset_of_first_event",
            &[11048, 12461],
            Packed {
                scale: Some(TIME_SCALE),
                offset: Some(TIME_OFFSET),
                fill: None,
                units: Some(TIME_UNITS),
            },
        );
        add_short(
            &mut file,
            "flash_energy",
            &[75, 8],
            Packed {
                scale: Some(FLASH_ENERGY_SCALE),
                offset: Some(ENERGY_OFFSET),
                fill: Some(-1),
                units: Some("J"),
            },
        );
    }
    let bytes = file.finish().expect("write");

    let parsed = parse_glm_netcdf(
        &bytes,
        GlmSatellite::GoesEast,
        &[GlmDataLevel::Event, GlmDataLevel::Flash],
    )
    .expect("the healthy level must still come through");

    // Half one: the good level survives.
    assert_eq!(parsed.records.len(), 2);
    assert!(parsed.records.iter().all(|r| r.level == GlmDataLevel::Event));

    // Half two: the broken level is *reported*. A bare `Ok` means
    // `parse_failures: None`, which the panel reads as "everything is fine".
    assert_eq!(parsed.level_failures.len(), 1, "the broken level must be reported");
    assert_eq!(parsed.level_failures[0].level, GlmDataLevel::Flash);
    assert_eq!(parsed.level_failures[0].satellite, GlmSatellite::GoesEast);
    assert!(
        parsed.level_failures[0].sample_error.contains("length mismatch"),
        "the report must carry why: {}",
        parsed.level_failures[0].sample_error
    );
}

/// The all-levels-failed case stays a *file* failure, so it keeps flowing into
/// the "N/M files failed to parse" count rather than becoming a level notice.
/// The two reports mean different things and must not be merged.
#[test]
fn every_level_failing_is_a_file_failure_not_a_level_failure() {
    let bytes = granule(&GranuleSpec {
        time_units: Some("fortnights since 2026-07-24 12:00:00.000"),
        ..Default::default()
    });
    let err = parse_glm_netcdf(&bytes, GlmSatellite::GoesEast, &[GlmDataLevel::Event])
        .expect_err("the only requested level failed, so the granule is unusable");
    assert!(err.contains("time units"), "the verbatim cause must survive: {err}");
}

/// A level with zero records is normal, not a failure: the product uses
/// unlimited dimensions, so a quiet 20-second granule genuinely carries
/// `number_of_flashes = 0` and must not trip the required-column check.
#[test]
fn a_level_with_no_records_is_empty_not_broken() {
    let mut file = new_granule(COVERAGE_START);
    {
        add_short(&mut file, "event_lat", &[], Packed {
            scale: Some(COORD_SCALE),
            offset: Some(LAT_OFFSET),
            fill: None,
            units: Some("degrees_north"),
        });
        add_short(&mut file, "event_lon", &[], Packed {
            scale: Some(COORD_SCALE),
            offset: Some(LON_OFFSET_EAST),
            fill: None,
            units: Some("degrees_east"),
        });
        add_short(&mut file, "event_time_offset", &[], Packed {
            scale: Some(TIME_SCALE),
            offset: Some(TIME_OFFSET),
            fill: None,
            units: Some(TIME_UNITS),
        });
        add_short(&mut file, "event_energy", &[], Packed {
            scale: Some(EVENT_ENERGY_SCALE),
            offset: Some(ENERGY_OFFSET),
            fill: Some(-1),
            units: Some("J"),
        });
    }
    let bytes = file.finish().expect("write");

    let parsed = parse_glm_netcdf(&bytes, GlmSatellite::GoesEast, &[GlmDataLevel::Event])
        .expect("an empty level is a quiet sky, not a broken product");
    assert!(parsed.records.is_empty());
    assert!(parsed.level_failures.is_empty(), "a quiet sky must not report a failure");
}

/// A healthy granule reports no level failures at all.
#[test]
fn a_healthy_granule_reports_no_level_failures() {
    let parsed = parse_glm_netcdf(
        &synthetic_granule(),
        GlmSatellite::GoesEast,
        &[GlmDataLevel::Event, GlmDataLevel::Flash],
    )
    .expect("parse");
    assert!(parsed.level_failures.is_empty());
}

/// Requesting several levels at once returns all of them.
#[test]
fn all_requested_levels_are_returned_together() {
    let bytes = synthetic_granule();
    let all = parse_glm_netcdf(
        &bytes,
        GlmSatellite::GoesEast,
        &[GlmDataLevel::Event, GlmDataLevel::Flash],
    )
    .expect("parse both levels")
    .records;
    assert_eq!(all.len(), 4); // 2 surviving events + 2 flashes
    assert_eq!(all.iter().filter(|f| f.level == GlmDataLevel::Event).count(), 2);
    assert_eq!(all.iter().filter(|f| f.level == GlmDataLevel::Flash).count(), 2);
}

// ---------------------------------------------------------------------------
// Real-granule regression, pinned to values the netCDF-C library produced.
// ---------------------------------------------------------------------------

/// A real GOES-19 GLM L2 LCFA granule: 148 flashes, 2172 groups, 5941 events,
/// with the `_Unsigned` packed shorts and the multi-chunk `group_lat` the
/// synthetic fixtures above cannot reproduce.
const REAL_GRANULE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/testdata/",
    "OR_GLM-L2-LCFA_G19_s20251801200000_e20251801200200_c20251801200212.nc"
);

/// FNV-1a over the IEEE bits of every unpacked value, with `None` distinct from
/// any real number. Exact rather than tolerance-based, so a single flipped
/// element in 5941 cannot hide behind an average.
pub(super) fn fingerprint(values: &[Option<f64>]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for x in values {
        for b in x.map_or(u64::MAX, f64::to_bits).to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// `(variable, element count, missing count, fingerprint, units)`.
///
/// **These numbers were produced by the netCDF-C library**, not by the reader
/// they check. Do not regenerate them with the pure-Rust reader — that would
/// assert only that the code agrees with itself.
///
/// The "missing" column is zero throughout: this granule carries no
/// `_FillValue` or out-of-range elements, so it does **not** exercise the
/// masking rules. Those are covered in `cf::tests`.
const GOLDEN: &[(&str, usize, usize, u64, &str)] = &[
    ("flash_lat", 148, 0, 0x25a7_2e49_ab40_a2c5, "degrees_north"),
    ("flash_lon", 148, 0, 0x737b_cf67_3941_900c, "degrees_east"),
    ("flash_energy", 148, 0, 0x2fa3_3f88_6b0b_4f99, "J"),
    ("flash_area", 148, 0, 0x29ae_d20c_3e8c_a70c, "m2"),
    (
        "flash_time_offset_of_first_event",
        148,
        0,
        0xe3a2_4c35_ca42_fc51,
        "seconds since 2025-06-29 12:00:00.000",
    ),
    ("group_lat", 2172, 0, 0x4700_9afb_e78e_734d, "degrees_north"),
    ("group_lon", 2172, 0, 0xbe30_5954_4c37_9d7a, "degrees_east"),
    ("group_energy", 2172, 0, 0xf92c_7b5c_fa74_c186, "J"),
    ("group_area", 2172, 0, 0xdd2a_2c57_5ebf_0126, "m2"),
    (
        "group_time_offset",
        2172,
        0,
        0xdcd6_8a01_211c_8742,
        "seconds since 2025-06-29 12:00:00.000",
    ),
    ("event_lat", 5941, 0, 0xdea7_1db9_6c3a_6b00, "degrees_north"),
    ("event_lon", 5941, 0, 0xf8c6_994a_9ef8_5382, "degrees_east"),
    ("event_energy", 5941, 0, 0x5e1d_5d72_11d7_dbac, "J"),
    (
        "event_time_offset",
        5941,
        0,
        0x5a7c_cef6_029a_8be2,
        "seconds since 2025-06-29 12:00:00.000",
    ),
];

/// The pure-Rust reader must reproduce, exactly, what the C library produced
/// on a real granule.
#[test]
fn real_granule_unpacks_to_the_values_the_c_netcdf_library_produced() {
    let bytes = std::fs::read(REAL_GRANULE).expect("read the committed GLM granule");
    let g = super::h5::Granule::open(&bytes).expect("open the granule");

    for (name, len, missing, hash, units) in GOLDEN {
        let v = g
            .read_unpacked(name)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .unwrap_or_else(|| panic!("{name} missing from the granule"));
        assert_eq!(v.values.len(), *len, "{name}: element count");
        assert_eq!(
            v.values.iter().filter(|x| x.is_none()).count(),
            *missing,
            "{name}: missing-value count"
        );
        assert_eq!(v.units.as_deref().unwrap_or(""), *units, "{name}: units");
        assert_eq!(
            fingerprint(&v.values),
            *hash,
            "{name}: unpacked values differ from what netCDF-C produced"
        );
    }
}

/// The fingerprints above still pass if every packed variable becomes a plain
/// float, so pin the storage too: these must stay `_Unsigned` 16-bit shorts
/// with a `scale_factor`, or the golden test checks an unused code path.
#[test]
fn the_real_granule_still_exercises_packed_unsigned_shorts() {
    use super::cf::VarType;

    let bytes = std::fs::read(REAL_GRANULE).expect("read the committed GLM granule");
    let g = super::h5::Granule::open(&bytes).expect("open the granule");

    for name in ["flash_energy", "flash_area", "event_lat", "event_lon"] {
        let v = g.raw_var(name).unwrap().unwrap();
        assert_eq!(v.vartype, VarType::SignedInt(2), "{name} storage");
        assert!(v.unsigned, "{name} lost _Unsigned");
        assert!(v.attrs.contains_key("scale_factor"), "{name} lost scale_factor");
    }

    // And `group_lat` must stay multi-chunk: it is the only variable here whose
    // chunk *placement* is observable, `flash_lat` being a single chunk at
    // offset 0.
    let f = hdf5_pure::File::from_bytes(bytes).unwrap();
    let ds = f.dataset("group_lat").unwrap();
    let chunk = ds.chunk_shape().unwrap().expect("group_lat chunk shape");
    assert!(2172u64.div_ceil(chunk[0]) >= 2, "group_lat is no longer multi-chunk");
}

/// The overlay end to end on a real granule: bytes off S3 in, plottable
/// lightning out.
///
/// Every assertion is a property of the physical world or the product spec, not
/// of the reader, so self-consistent nonsense (CF unpacking's failure mode)
/// cannot satisfy it.
#[test]
fn a_real_granule_parses_into_plottable_lightning() {
    let bytes = std::fs::read(REAL_GRANULE).expect("read the committed GLM granule");
    let parsed = parse_glm_netcdf(
        &bytes,
        GlmSatellite::GoesEast,
        &[GlmDataLevel::Flash, GlmDataLevel::Group, GlmDataLevel::Event],
    )
    .expect("a real NOAA granule must parse");

    assert!(parsed.level_failures.is_empty(), "no level may fail on a healthy granule");

    // 148 flashes + 2172 groups + 5941 events, and nothing dropped: every
    // record in this granule is inside the globe check in `parse_level_records`.
    assert_eq!(parsed.records.len(), 8261, "record count");
    for (level, want) in [
        (GlmDataLevel::Flash, 148),
        (GlmDataLevel::Group, 2172),
        (GlmDataLevel::Event, 5941),
    ] {
        let n = parsed.records.iter().filter(|r| r.level == level).count();
        assert_eq!(n, want, "{level:?} count");
    }

    // The granule's `time_coverage_start` is 2025-06-29 12:00:00 and its time
    // axis spans exactly [-5 s, +20 s] around that: every `*_time_offset` is a
    // u16 with `scale_factor = 0.0003814756` and `add_offset = -5.0`, so
    // `0 -> -5.000` and `65535 -> +20.000`.
    //
    // The negative lower bound is the product, not a bug: a flash is a temporal
    // cluster whose first event can fall before the granule boundary. This
    // granule holds such a record at 11:59:58.967.
    let start = chrono::NaiveDate::from_ymd_opt(2025, 6, 29)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap();
    let first = start - chrono::Duration::milliseconds(5_001);
    let end = start + chrono::Duration::milliseconds(20_001);

    for r in &parsed.records {
        assert!(r.lat.is_finite() && r.lon.is_finite(), "non-finite position");
        assert!((-90.0..=90.0).contains(&r.lat), "latitude off the globe: {}", r.lat);
        assert!((-180.0..=180.0).contains(&r.lon), "longitude off the globe: {}", r.lon);
        // GOES-East sits over -75.2°. Catches an `add_offset` skipped or
        // applied twice: the values stay finite and in-range, they just land in
        // the wrong ocean.
        assert!(
            (-160.0..=10.0).contains(&r.lon),
            "outside GOES-East's field of view: {}",
            r.lon
        );
        assert!(
            r.time >= first && r.time <= end,
            "timestamp outside the granule's time axis: {}",
            r.time
        );

        if let Some(e) = r.energy {
            // GLM reports radiant energy around 1e-15 J. A missing
            // `scale_factor` puts this at ~1e4 (the raw count), a doubled one
            // at ~1e-31.
            assert!(
                (1e-16..1e-10).contains(&e),
                "energy outside the range GLM reports: {e}"
            );
        }
        if let Some(a) = r.area {
            // Groups and flashes span tens to thousands of km²; the packed
            // count is in `m2` and must have been converted.
            assert!((1.0..1e6).contains(&a), "area outside the plausible range: {a} km2");
        }
    }

    // Events carry no area (the product has no `event_area`), flashes and
    // groups do.
    assert!(
        parsed
            .records
            .iter()
            .filter(|r| r.level == GlmDataLevel::Event)
            .all(|r| r.area.is_none()),
        "events must not report an area"
    );
    assert!(
        parsed
            .records
            .iter()
            .filter(|r| r.level == GlmDataLevel::Flash)
            .all(|r| r.area.is_some()),
        "flashes must report an area"
    );
}
