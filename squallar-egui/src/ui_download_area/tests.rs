//! Tests for [`super`].
//!
//! The load-bearing ones are the three that could go quietly wrong: the figure
//! is the **same** number `pmt_index` answers with and it **moves** when the
//! level does (a figure that never moves is the tell for a stale or estimated
//! one); the deepest level's zoom is the archive header's own, asserted
//! against a fixture whose `max_zoom` is **10**, so a hardcoded 14 fails here;
//! and the frame-facing entry point does no archive work at all.

use std::path::{Path, PathBuf};

use squallar_units::DataSize;

use super::*;
use crate::basemap_archive::FileRangeSource;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The committed Monaco build: `max_zoom` 14, both dedup mechanisms live.
const MONACO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/monaco.pmtiles");

/// The hand-built raster mini archive. **`max_zoom` is 10, not 14** — which is
/// the whole reason it is here: a level table that hardcoded the shipped
/// archive's ceiling would pass against Monaco and fail against this.
const TERRAIN_MINI: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/testdata/terrain-hillshade-mini.pmtiles"
);

/// Monaco's own ground: the fixture covers the principality, so a box here
/// addresses tiles the archive actually holds at every level.
const MONACO_CENTRE: squallar_geo::GeoPoint = squallar_geo::GeoPoint {
    lat: 43.7384,
    lon: 7.4246,
};

/// Run `future` on a current-thread runtime — `pmt_index::tests`' helper, for
/// the oracle half of the exactness test.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime should build")
        .block_on(future)
}

fn source(path: &str) -> FileRangeSource {
    FileRangeSource::open(Path::new(path))
        .unwrap_or_else(|error| panic!("the committed fixture {path} must open: {error}"))
}

/// How long a probe is given to land every figure it was asked for. Generous
/// on purpose: the work is a real file read on a real IO thread, and a tight
/// budget here would make an unrelated slow machine look like a defect.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Drive `probe` until `done` answers true, or fail saying what it was still
/// waiting for.
///
/// `terrain` is the archive the probe reads the hillshade half out of. A probe
/// with the checkbox clear never reaches it — which is itself the pin that a
/// basemap-only download opens no terrain archive.
fn pump(
    probe: &mut AreaSizeProbe,
    path: &str,
    terrain: &str,
    what: &str,
    done: impl Fn(&AreaSizeProbe) -> bool,
) {
    let ctx = egui::Context::default();
    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    while std::time::Instant::now() < deadline {
        probe.poll(&ctx, || Some(source(path)), || Some(source(terrain)));
        if done(probe) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the probe never {what} within {PROBE_TIMEOUT:?}");
}

/// A probe pointed at `path` with `picked` in hand, driven until all three
/// levels have their figure.
fn measured(path: &'static str, picked: PickedBox) -> AreaSizeProbe {
    measured_over(path, MONACO, picked)
}

/// [`measured`] with the terrain archive named too — for the combined figure.
fn measured_over(path: &str, terrain: &str, picked: PickedBox) -> AreaSizeProbe {
    let mut probe = AreaSizeProbe::new();
    probe.set_box(Some(picked));
    pump(
        &mut probe,
        path,
        terrain,
        "read the archive's ceiling",
        |probe| probe.ceiling().is_some(),
    );
    pump(
        &mut probe,
        path,
        terrain,
        "measured all three levels",
        |probe| probe.sizes().len() == DETAIL_LEVELS.len(),
    );
    probe
}

// ---------------------------------------------------------------------------
// The drag becomes an area
// ---------------------------------------------------------------------------

/// **The arm's whole shape**: a box the drag described becomes a bbox and a
/// zoom, with no voxel resampler anywhere in it.
#[test]
fn the_box_the_drag_described_becomes_the_area_that_is_downloaded() {
    let picked = PickedBox::new(MONACO_CENTRE, 20.0).expect("a 40 km box is a box");
    let spec = picked
        .area_spec(14, DetailLevel::EveryStreet)
        .expect("a box on real ground has a bbox");

    assert_eq!(spec.max_zoom, 14, "the deepest level stores to the ceiling");
    assert!(
        spec.west < MONACO_CENTRE.lon && MONACO_CENTRE.lon < spec.east,
        "the bbox does not straddle its own centre's longitude: {spec:?}"
    );
    assert!(
        spec.south < MONACO_CENTRE.lat && MONACO_CENTRE.lat < spec.north,
        "the bbox does not straddle its own centre's latitude: {spec:?}"
    );

    let (nw, se) = crate::ui_region::corners_for(MONACO_CENTRE, picked.half_extent())
        .expect("the same corner math the box is painted through");
    assert_eq!(
        (spec.west, spec.south, spec.east, spec.north),
        (nw.lon, se.lat, se.lon, nw.lat),
        "the area's bbox is not the corners the box was drawn at",
    );
    crate::basemap_download::valid_area_id(&spec.area_id)
        .expect("the area id must be one the stores will build a filename from");
}

/// **The decoupling's whole point.** A box under the voxel resampler's 10 km
/// floor is an ordinary download area; the same drag, with the 3D pick's
/// bounds, would refuse it outright.
#[test]
fn a_town_sized_box_the_voxel_bounds_would_refuse_downloads_fine() {
    const HALF_KM: f64 = 3.0; // a 6 km box - a town
    const {
        assert!(
            HALF_KM < squallar_radar::voxel::MIN_HALF_WIDTH_KM,
            "this test is vacuous unless the box really is under the resampler's floor"
        );
    }

    let pane = 0;
    let mut refused = crate::ui_region::RegionDrag::begin(
        pane,
        MONACO_CENTRE,
        crate::ui_region::DragBoundsKm {
            min_half_width_km: squallar_radar::voxel::MIN_HALF_WIDTH_KM,
            max_half_width_km: squallar_radar::voxel::MAX_HALF_WIDTH_KM,
        },
    )
    .expect("a drag begins on real ground");
    let mut accepted =
        crate::ui_region::RegionDrag::begin(pane, MONACO_CENTRE, download_pick_bounds())
            .expect("a drag begins on real ground");

    let corner = squallar_geo::GeoPoint {
        lat: MONACO_CENTRE.lat + HALF_KM / squallar_geo::KM_PER_DEGREE_LAT,
        lon: MONACO_CENTRE.lon,
    };
    refused.extend_to(corner);
    accepted.extend_to(corner);

    assert!(
        refused.commit().is_none(),
        "the 3D pick's bounds accepted a box under their own minimum"
    );
    let (centre, half_width_km) = accepted
        .commit()
        .expect("the download arm's bounds must accept a town-sized box");
    assert!(
        (half_width_km - HALF_KM).abs() < 0.05,
        "the download arm committed {half_width_km} km rather than the {HALF_KM} km dragged"
    );
    let picked = PickedBox::new(centre, half_width_km).expect("a committed box is a box");
    assert!(
        picked.area_spec(14, DetailLevel::EveryStreet).is_some(),
        "a town-sized box produced no area"
    );
}

/// Two depths of one box are two downloads, because a resume is a set
/// difference over a *plan*'s segments and two plans number them differently.
#[test]
fn one_box_at_two_depths_is_two_areas() {
    let picked = PickedBox::new(MONACO_CENTRE, 20.0).expect("a box");
    let shallow = picked
        .area_spec(14, DetailLevel::CitiesAndHighways)
        .expect("a spec");
    let deep = picked
        .area_spec(14, DetailLevel::EveryStreet)
        .expect("a spec");
    assert_ne!(
        shallow.area_id, deep.area_id,
        "two depths share an id, so a resume could graft one plan's segment onto the other's"
    );
    let again = picked
        .area_spec(14, DetailLevel::EveryStreet)
        .expect("a spec");
    assert_eq!(
        deep.area_id, again.area_id,
        "the same box at the same depth did not answer with the same id, so re-picking it \
         would duplicate the download rather than resume it"
    );
}

// ---------------------------------------------------------------------------
// The detail levels
// ---------------------------------------------------------------------------

/// **The ceiling is the archive's, and the fixture's is 10.** A table that
/// hardcoded the shipped archive's 14 passes against Monaco and fails here.
#[test]
fn the_deepest_level_stores_to_the_archives_own_ceiling() {
    let header_ceiling = block_on(async {
        crate::pmt_index::PmtIndex::open(source(TERRAIN_MINI))
            .await
            .expect("the committed mini archive must open")
            .header()
            .max_zoom
    });
    assert_eq!(
        header_ceiling, 10,
        "the fixture this test exists to be non-14 is no longer non-14"
    );

    let picked = PickedBox::new(
        squallar_geo::GeoPoint {
            lat: 41.0,
            lon: -101.0,
        },
        5.0,
    )
    .expect("a box");
    let probe = {
        let mut probe = AreaSizeProbe::new();
        probe.set_box(Some(picked));
        pump(
            &mut probe,
            TERRAIN_MINI,
            TERRAIN_MINI,
            "read the ceiling",
            |probe: &AreaSizeProbe| probe.ceiling().is_some(),
        );
        probe
    };

    assert_eq!(
        probe.ceiling(),
        Some(header_ceiling),
        "the probe's ceiling is not the header's"
    );
    assert_eq!(
        probe
            .area_spec(DetailLevel::EveryStreet)
            .expect("a spec once the ceiling is known")
            .max_zoom,
        header_ceiling,
        "the deepest level stored past or short of the archive's own ceiling"
    );
    assert_ne!(
        header_ceiling, 14,
        "a hardcoded 14 would have passed this assertion"
    );
}

/// The three levels are three depths, evenly stepped below the ceiling, and
/// they reproduce the design table's z10 / z12 / z14 on the shipped archive.
#[test]
fn the_three_levels_step_evenly_below_the_ceiling() {
    assert_eq!(
        DETAIL_LEVELS.map(|level| level.zoom_in(14)),
        [10, 12, 14],
        "against the shipped ceiling the levels must land on the design table's zooms"
    );
    assert_eq!(
        DETAIL_LEVELS.map(|level| level.zoom_in(16)),
        [12, 14, 16],
        "a deeper archive must make every level deeper, not only the top one"
    );
    assert_eq!(
        DetailLevel::EveryStreet.zoom_in(10) - DetailLevel::TownsAndMainRoads.zoom_in(10),
        crate::basemap_areas::DETAIL_LEVEL_STEP,
        "the step between levels is not the step the constant names"
    );
    for level in DETAIL_LEVELS {
        assert!(
            !level.label().chars().any(|c| c.is_ascii_digit()),
            "{:?}'s name carries a number; detail is framed by what you can make out",
            level
        );
    }
}

/// Every level's persisted token round-trips, and a token this build has no
/// level for costs the choice rather than the file.
#[test]
fn a_levels_token_round_trips_and_an_unknown_one_is_dropped() {
    for level in DETAIL_LEVELS {
        assert_eq!(DetailLevel::from_token(level.token()), Some(level));
    }
    assert_eq!(DetailLevel::from_token("streets_and_buildings"), None);
}

// ---------------------------------------------------------------------------
// The figure is exact, and it moves
// ---------------------------------------------------------------------------

/// **The exactness identity, and the movement that proves it is not an
/// estimate.**
///
/// The probe's figure for each level equals `pmt_index`'s own
/// `download_bytes` over the same tiles — the distinct `(offset, length)` sum,
/// not `tile_count x average` — and the three figures are three *different*
/// numbers. A figure that did not move with the level would pass an equality
/// test against a cached first answer while telling the user nothing.
#[test]
fn the_live_figure_is_pmt_indexs_exact_figure_and_moves_with_the_level() {
    let picked = PickedBox::new(MONACO_CENTRE, 8.0).expect("a 16 km box over Monaco");
    let probe = measured(MONACO, picked);

    let ceiling = probe.ceiling().expect("the ceiling landed");
    for level in DETAIL_LEVELS {
        let spec = probe.area_spec(level).expect("a spec");
        let oracle = block_on(async {
            let index = crate::pmt_index::PmtIndex::open(source(MONACO))
                .await
                .expect("the fixture opens");
            index
                .download_bytes(crate::basemap_download::area_tiles(&spec))
                .await
                .expect("the fixture measures")
        });
        assert_eq!(
            probe.figure(level),
            Some(oracle),
            "{:?} (z0..={}) drew a figure that is not pmt_index's own",
            level,
            level.zoom_in(ceiling),
        );
    }

    let sizes: Vec<DataSize> = DETAIL_LEVELS
        .into_iter()
        .map(|level| probe.size(level).expect("every level measured"))
        .collect();
    assert!(
        sizes[0] < sizes[1] && sizes[1] < sizes[2],
        "the three levels quoted {sizes:?}; a figure that does not move with the detail is \
         the tell for a stale or estimated one",
    );
    assert!(
        sizes[2] > DataSize::ZERO,
        "the deepest level over Monaco's own ground measured nothing at all"
    );
}

/// The probe does not blank what it has already measured while it measures
/// something else, and it stops working on a box nobody is looking at.
#[test]
fn measured_figures_survive_a_box_being_dropped_and_picked_up_again() {
    let picked = PickedBox::new(MONACO_CENTRE, 8.0).expect("a box");
    let mut probe = measured(MONACO, picked);
    let before = probe.sizes();
    assert_eq!(before.len(), 3, "the setup did not measure all three");

    probe.set_box(None);
    assert!(
        probe.sizes().is_empty(),
        "with no box in hand there is nothing to quote a size for"
    );

    probe.set_box(Some(picked));
    assert_eq!(
        probe.sizes(),
        before,
        "the same box re-picked re-measured from scratch instead of answering from what it \
         had already read"
    );
}

// ---------------------------------------------------------------------------
// The frame thread
// ---------------------------------------------------------------------------

/// A named function's body, read out of a source file this crate ships — the
/// `egui_frame_pin_tests` construction.
fn body_of(source: &'static str, signature: &str) -> &'static str {
    source
        .split_once(signature)
        .and_then(|(_, rest)| rest.split_once("\n    }"))
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("`{signature}` is no longer a function there"))
}

/// **The off-frame-thread pin.**
///
/// The frame path's only entry into this module is [`AreaSizeProbe::poll`], so
/// what has to be true is that its body neither awaits, nor blocks, nor opens
/// an archive, nor sums a byte — it may only read a slot and hand a task to
/// the runtime. Spelled as a source pin rather than a timing assertion,
/// because a clock would red-gate on an unrelated slow machine and would pass
/// on a fast one that had regressed.
#[test]
fn the_frame_facing_poll_does_no_archive_work() {
    let module = include_str!("../ui_download_area.rs");
    let body = body_of(module, "    pub(crate) fn poll<S, T, F, G>(");

    for forbidden in [".await", "block_on", "download_bytes", "PmtIndex::open"] {
        assert!(
            !body.contains(forbidden),
            "AreaSizeProbe::poll now contains `{forbidden}`, which puts archive work on the \
             frame thread; the read belongs inside the spawned task"
        );
    }
    assert!(
        body.contains("runtime::spawn"),
        "AreaSizeProbe::poll no longer hands its work to the IO runtime, so wherever the \
         work went it is not off the frame thread by construction"
    );

    // The sum lives in exactly one place, and that place is the async task.
    assert_eq!(
        module.matches("download_bytes(").count(),
        1,
        "there is more than one place this module sums bytes; the pin above only covers the \
         one inside `measure`"
    );
    let measure = body_of(module, "async fn measure<S: ArchiveRangeSource>(");
    assert!(
        measure.contains("download_bytes("),
        "the byte sum has left `measure`, so the pin above is measuring the wrong function"
    );

    // And the frame path itself never names the index.
    let frame = include_str!("../ui_map.rs");
    let pump = body_of(frame, "    pub(super) fn pump_download_area(");
    for forbidden in [".await", "block_on", "download_bytes", "PmtIndex"] {
        assert!(
            !pump.contains(forbidden),
            "`pump_download_area` runs on the frame thread and now contains `{forbidden}`"
        );
    }
}

// ---------------------------------------------------------------------------
// Quota
// ---------------------------------------------------------------------------

fn mb(n: u64) -> DataSize {
    DataSize::from_bytes(n * 1_000_000)
}

fn three_sizes() -> Vec<(DetailLevel, DataSize)> {
    vec![
        (DetailLevel::CitiesAndHighways, mb(12)),
        (DetailLevel::TownsAndMainRoads, mb(47)),
        (DetailLevel::EveryStreet, mb(310)),
    ]
}

/// **Quantities and an action, never an apology.** A level that will not fit
/// yields both figures and the deepest level that does.
#[test]
fn a_level_that_will_not_fit_names_both_quantities_and_a_level_that_does() {
    let short = quota_shortfall(&three_sizes(), DetailLevel::EveryStreet, Some(mb(180)))
        .expect("310 MB does not fit in 180 MB");

    assert_eq!(short.needs, mb(310));
    assert_eq!(short.free, mb(180));
    assert_eq!(
        short.alternative,
        Some(DetailLevel::TownsAndMainRoads),
        "the alternative must be the DEEPEST level that fits, not the smallest"
    );

    let line = shortfall_line(short);
    assert!(
        line.contains(&mb(310).label()) && line.contains(&mb(180).label()),
        "the shortfall line {line:?} does not carry both quantities"
    );
    assert!(
        shortfall_action_label(DetailLevel::TownsAndMainRoads)
            .contains(DetailLevel::TownsAndMainRoads.label()),
        "the action does not name the level it would switch to"
    );
}

/// An unknown quota is not a refusal, and a level that fits is not a
/// shortfall.
#[test]
fn an_unknown_quota_refuses_nothing() {
    assert_eq!(
        quota_shortfall(&three_sizes(), DetailLevel::EveryStreet, None),
        None,
        "an unknown quota was rendered as a shortfall, which is a warning the user cannot act on"
    );
    assert_eq!(
        quota_shortfall(
            &three_sizes(),
            DetailLevel::CitiesAndHighways,
            Some(mb(180))
        ),
        None,
        "12 MB in 180 MB free was reported short"
    );
    assert_eq!(
        quota_shortfall(&[], DetailLevel::EveryStreet, Some(mb(1))),
        None,
        "a level with no measured figure cannot be short of anything"
    );

    let none_fit = quota_shortfall(&three_sizes(), DetailLevel::EveryStreet, Some(mb(1)))
        .expect("310 MB does not fit in 1 MB");
    assert_eq!(
        none_fit.alternative, None,
        "a level was offered that does not fit either"
    );
    assert!(
        shortfall_line(none_fit).contains(&mb(1).label()),
        "even with no alternative the line must still state what there is"
    );
}

/// The free-space figure is a subtraction of two real numbers or it is
/// nothing.
#[test]
fn free_space_is_never_invented() {
    use crate::basemap_download::OfflineQuota;

    assert_eq!(free_space(None), None);
    assert_eq!(
        free_space(Some(OfflineQuota {
            usage: None,
            quota: Some(1_000)
        })),
        None,
        "an unknown usage was treated as zero"
    );
    assert_eq!(
        free_space(Some(OfflineQuota {
            usage: Some(400),
            quota: None
        })),
        None,
        "an unknown quota was treated as unlimited"
    );
    assert_eq!(
        free_space(Some(OfflineQuota {
            usage: Some(400),
            quota: Some(1_000)
        })),
        Some(DataSize::from_bytes(600))
    );
    assert_eq!(
        free_space(Some(OfflineQuota {
            usage: Some(2_000),
            quota: Some(1_000)
        })),
        Some(DataSize::ZERO),
        "a usage over quota must read as nothing free, not underflow"
    );
}

/// Nothing above depends on a path that does not exist.
#[test]
fn the_fixtures_this_suite_reads_are_committed() {
    for path in [MONACO, TERRAIN_MINI] {
        let path = PathBuf::from(path);
        let bytes = std::fs::metadata(&path)
            .unwrap_or_else(|error| panic!("{} must be committed: {error}", path.display()))
            .len();
        assert!(bytes > 0, "{} is empty", path.display());
    }
}

// ---------------------------------------------------------------------------
// The combined figure
// ---------------------------------------------------------------------------

/// A raster archive over `spec` down to `max_zoom`, written where the probe
/// can open it as a file — the terrain half of the combined figure.
///
/// Built rather than committed because what it must be is *specific*: it has
/// to cover the same ground the basemap fixture does (or the terrain half
/// would quote zero and the sum would be indistinguishable from the basemap
/// alone) and stop at a ceiling that is neither 12 nor 14.
struct TerrainFixture(PathBuf);

impl TerrainFixture {
    fn new(test: &str, spec: &AreaSpec, max_zoom: u8) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "squallar-probe-terrain-{}-{test}.pmtiles",
            std::process::id()
        ));
        std::fs::write(
            &path,
            crate::basemap_download::tests::raster_archive(spec, max_zoom),
        )
        .expect("the terrain fixture writes");
        Self(path)
    }

    fn path(&self) -> &str {
        self.0.to_str().expect("a temp path is UTF-8")
    }
}

impl Drop for TerrainFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The ceiling the terrain fixture stops at. **Neither 12 (the shipped
/// hillshade build's) nor 14 (the basemap's)**, so a figure enumerated against
/// either constant disagrees with this fixture.
const PROBE_TERRAIN_CEILING: u8 = 9;

/// `pmt_index`'s own figure for `spec` out of the archive at `path`, clamped
/// to **that archive's own** header ceiling — the oracle, computed the way the
/// engine enumerates and by a path that shares no arithmetic with the probe.
fn oracle(path: &str, spec: &AreaSpec) -> crate::pmt_index::DownloadBytes {
    block_on(async {
        let index = crate::pmt_index::PmtIndex::open(source(path))
            .await
            .expect("the fixture opens");
        let ceiling = index.header().max_zoom;
        index
            .download_bytes(crate::basemap_download::area_tiles_to(spec, ceiling))
            .await
            .expect("the fixture measures")
    })
}

/// **The combined figure is exact and it MOVES when the checkbox flips.**
///
/// Exact: the figure with terrain on is the two archives' own
/// `download_bytes` totals added — each its distinct `(offset, length)` sum
/// over that archive's own tiles, clamped to that archive's own ceiling. Not
/// scaled, not estimated, and not the basemap's figure with a factor on it.
///
/// Moves: the same box with the checkbox clear quotes the basemap's figure
/// alone. A figure that did not move is the tell for an estimate — and it
/// would also be the tell for a sum that silently dropped the second archive.
#[test]
fn the_combined_figure_is_both_archives_exact_totals_and_moves_with_the_checkbox() {
    const TEST: &str = "the_combined_figure_is_both_archives_exact_totals_and_moves";
    let picked = PickedBox::new(MONACO_CENTRE, 8.0).expect("a 16 km box over Monaco");
    // The deepest level's own spec is what the fixture is cut for, so every
    // level's tiles fall inside it.
    let deepest = picked
        .area_spec(14, DetailLevel::EveryStreet)
        .expect("a spec");
    let terrain = TerrainFixture::new(TEST, &deepest, PROBE_TERRAIN_CEILING);

    // The precondition that makes the ceiling half of this falsifiable.
    let terrain_ceiling = block_on(async {
        crate::pmt_index::PmtIndex::open(source(terrain.path()))
            .await
            .expect("the built fixture opens")
            .header()
            .max_zoom
    });
    assert_eq!(terrain_ceiling, PROBE_TERRAIN_CEILING);
    assert_ne!(terrain_ceiling, 12, "a hardcoded 12 would have passed this");
    assert_ne!(terrain_ceiling, 14, "a hardcoded 14 would have passed this");

    // With the checkbox clear: the basemap's own figure, nothing added.
    let basemap_only = measured_over(MONACO, terrain.path(), picked);
    for level in DETAIL_LEVELS {
        let spec = basemap_only.area_spec(level).expect("a spec");
        assert_eq!(
            basemap_only.figure(level),
            Some(oracle(MONACO, &spec)),
            "{level:?} without terrain drew a figure that is not the basemap's own"
        );
    }

    // With it ticked: the two archives' own figures, summed.
    let mut both = AreaSizeProbe::new();
    both.set_box(Some(picked));
    both.set_terrain(true);
    pump(
        &mut both,
        MONACO,
        terrain.path(),
        "read the ceiling",
        |probe: &AreaSizeProbe| probe.ceiling().is_some(),
    );
    pump(
        &mut both,
        MONACO,
        terrain.path(),
        "measured all three levels over both archives",
        |probe: &AreaSizeProbe| probe.sizes().len() == DETAIL_LEVELS.len(),
    );

    for level in DETAIL_LEVELS {
        let spec = both.area_spec(level).expect("a spec");
        let base = oracle(MONACO, &spec);
        let hill = oracle(terrain.path(), &spec);
        assert!(
            base.bytes > 0 && hill.bytes > 0,
            "{level:?}: a half quoting zero makes the sum indistinguishable from the \
             other half alone ({} + {})",
            base.bytes,
            hill.bytes
        );
        let expected = crate::pmt_index::DownloadBytes {
            bytes: base.bytes + hill.bytes,
            present: base.present + hill.present,
            absent: base.absent + hill.absent,
        };
        assert_eq!(
            both.figure(level),
            Some(expected),
            "{level:?} with terrain drew something other than the two archives' own \
             totals summed"
        );

        // And it moved. The detail levels are still named against the BASE
        // MAP's ceiling, which is what makes the two probes comparable at all.
        assert_eq!(
            basemap_only.area_spec(level),
            both.area_spec(level),
            "ticking the checkbox renamed the level's own area"
        );
        assert!(
            both.size(level).expect("measured") > basemap_only.size(level).expect("measured"),
            "{level:?}: the figure did not move when the checkbox was ticked, which is \
             the tell for an estimate or for a dropped archive"
        );
    }

    assert_eq!(
        both.ceiling(),
        Some(14),
        "the terrain archive's shallower ceiling moved the detail levels, which are \
         the base map's own depth"
    );
}

/// Clearing the checkbox gives the basemap figure **back**, not a stale sum —
/// and it costs no second read, because both halves are still remembered.
#[test]
fn clearing_the_checkbox_returns_the_basemap_figure_alone() {
    const TEST: &str = "clearing_the_checkbox_returns_the_basemap_figure_alone";
    let picked = PickedBox::new(MONACO_CENTRE, 8.0).expect("a 16 km box over Monaco");
    let deepest = picked
        .area_spec(14, DetailLevel::EveryStreet)
        .expect("a spec");
    let terrain = TerrainFixture::new(TEST, &deepest, PROBE_TERRAIN_CEILING);

    let mut probe = AreaSizeProbe::new();
    probe.set_box(Some(picked));
    probe.set_terrain(true);
    pump(
        &mut probe,
        MONACO,
        terrain.path(),
        "read the ceiling",
        |probe: &AreaSizeProbe| probe.ceiling().is_some(),
    );
    pump(
        &mut probe,
        MONACO,
        terrain.path(),
        "measured both halves",
        |probe: &AreaSizeProbe| probe.sizes().len() == DETAIL_LEVELS.len(),
    );

    let level = DetailLevel::TownsAndMainRoads;
    let combined = probe.size(level).expect("measured with terrain");
    probe.set_terrain(false);
    let alone = probe
        .size(level)
        .expect("the basemap half is still remembered");
    assert!(
        alone < combined,
        "clearing the checkbox kept the combined figure: {alone:?} vs {combined:?}"
    );
    let spec = probe.area_spec(level).expect("a spec");
    assert_eq!(
        probe.figure(level),
        Some(oracle(MONACO, &spec)),
        "the figure after clearing is not the basemap's own"
    );

    // Ticking it again answers from memory: no read runs, so the figure is
    // back on the very next question rather than after another archive walk.
    probe.set_terrain(true);
    assert_eq!(
        probe.size(level),
        Some(combined),
        "re-ticking threw away the half already measured"
    );
}
