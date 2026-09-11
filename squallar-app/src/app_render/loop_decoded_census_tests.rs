//! **What the loop's decoded cache is holding that nothing can evict, and what
//! dropping it would actually cost the reader.**
//!
//! Two questions, and they are asked here together because the answer to the
//! second is what makes the first worth measuring.
//!
//! * `evict_decoded_except` and `evict_decoded_to_ceiling` both refuse a volume
//!   with no archive behind it — a chunk-feed arrival, whose S3 object does not
//!   exist yet — so the residency pass can decide it does not want a frame's
//!   moments and be unable to act. `App::loop_decoded_census` is the level that
//!   says how much of the cache is in that state; the tests below are what keep
//!   it from being a zero instrument.
//! * And the price of ever letting go of one: a loop frame that is already
//!   textured holds its picture and its readout in allocations of its own, so
//!   the volume behind it can go without the reader losing the frame. That is
//!   `a_textured_frames_picture_and_readout_outlive_the_volume_it_was_drawn_from`,
//!   which drops the volume through the one path that can take an archive-less
//!   one today and then reads the picture and the number back.

use super::loop_dispatch_tests::volume_with_sweeps;
use super::radar_timeline_addressing_tests::{active_loop, at, point_at_site, textured};
use super::*;
use crate::app::tests::{drain_uploads, headless};
use crate::platform_double::TestBridge;
use squallar_source::id::known;

const SITE: &str = "KTLX";
const TILT: f32 = 0.5;
/// The one gate the fixture volume carries, in the fixed-point encoding
/// `MomentData::from_fixed_point` decodes with `(raw - offset) / scale`.
const GATE_RAW: u8 = 100;
const GATE_SCALE: f32 = 2.0;
const GATE_OFFSET: f32 = 66.0;
const GATE_VALUE: f32 = (GATE_RAW as f32 - GATE_OFFSET) / GATE_SCALE;

/// A one-sweep, one-radial, one-gate volume whose gate carries a **painted**
/// value: `scan_with_sweeps`' gate is raw 0, which the wire spells "below
/// threshold", and a readout assertion over that would pass on a source
/// holding nothing.
fn volume_with_a_painted_gate() -> Arc<nexrad_model::data::Scan> {
    use nexrad_model::data::{MomentData, Radial, RadialStatus, Scan, Sweep};
    let radial = Radial::new(
        0,
        0,
        0.0,
        1.0,
        RadialStatus::ElevationStart,
        1,
        TILT,
        Some(MomentData::from_fixed_point(
            1,
            0,
            250,
            8,
            GATE_SCALE,
            GATE_OFFSET,
            vec![GATE_RAW],
        )),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let scan = Scan::new(
        nexrad_model::data::VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            nexrad_model::data::PulseWidth::Short,
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
        vec![Sweep::new(1, vec![radial])],
    );
    Arc::new(scan)
}

/// The geometry a loop frame's render hands back: **wedges and no values**.
/// A loop render strips its numbers (`PolarField::strip_values`) precisely so
/// the readout falls through to the sweep, which is the ownership this module
/// is about; a field carrying values would answer from itself and the test
/// would say nothing.
fn geometry_over_one_gate() -> squallar_radar::render::polar::PolarField {
    let geometry = squallar_radar::render::polar::PolarGeometry::from_parts(
        vec![squallar_radar::render::polar::Wedge {
            azimuth_deg: 0.0,
            half_width_deg: 0.5,
        }],
        0.25,
        0.25,
        // `None`: the two ranges above are already ground ranges, so the
        // readout's pick needs no beam conversion.
        None,
        1,
    );
    squallar_radar::render::polar::PolarField::from_parts(geometry, Vec::new())
}

/// One pane on [`SITE`] looping over `stamps`, with the volume for each stamp
/// in the loop cache and **no archive behind any of them** — the chunk feed's
/// shape, which is the only arrival `append_scan_to_active_loops` files.
fn looping_pane(stamps: &[chrono::NaiveDateTime]) -> crate::app::App {
    let mut app = headless(TestBridge::desktop());
    point_at_site(&mut app, 0);
    app.loop_mgr = squallar_radar::loop_downloads::LoopDownloadManager::new();
    for &stamp in stamps {
        app.loop_mgr
            .cache_scan(SITE, stamp, volume_with_sweeps(&[TILT]));
    }
    *app.gui
        .pane_mut(0)
        .expect("a headless app has a pane")
        .time_state_mut(&known::RADAR) = active_loop(stamps);
    app
}

fn frame_picture(
    app: &crate::app::App,
    frame: usize,
) -> Option<&squallar_egui::pane::RadarImageData> {
    app.gui
        .pane(0)?
        .time_state(&known::RADAR)
        .frames
        .get(frame)?
        .image
        .as_ref()?
        .plan_view()
}

/// **The whole price of the ruling, demonstrated rather than argued.**
///
/// A frame is rendered through the real arrival path — the render lands on
/// `loop_render_sender`, `poll_loop_render_results` uploads it and builds the
/// hover source off the volume in the loop cache — and then the volume is
/// dropped through `retain_scans`, the one path that takes an archive-less
/// volume today. After the drop:
///
/// * the frame still holds its texture, the same id, and egui still has it —
///   so the loop plays with no gap where that frame is;
/// * the readout still answers the same number, because `SweepGates` cloned
///   the drawn sweep's moments out and keeps no reference to the volume.
///
/// What the reader loses is what the volume alone can do: a retarget to
/// another product or tilt, which `retarget_renders` blanks every frame for
/// and which has nothing to re-render this one from until the archive exists.
#[test]
fn a_textured_frames_picture_and_readout_outlive_the_volume_it_was_drawn_from() {
    let ctx = egui::Context::default();
    let mut app = looping_pane(&[at(1)]);
    app.loop_mgr
        .cache_scan(SITE, at(1), (volume_with_a_painted_gate(), Arc::default()));
    app.gui
        .pane_mut(0)
        .expect("a headless app has a pane")
        .time_state_mut(&known::RADAR)
        .frames[0]
        .render_in_flight = true;
    let target = app
        .gui
        .pane(0)
        .expect("a headless app has a pane")
        .time_state(&known::RADAR)
        .rendered_for
        .clone()
        .expect("the fixture loop is keyed");
    drain_uploads(&ctx);

    app.channels
        .loop_render_sender
        .send(crate::channels::LoopRenderResponse {
            pane_idx: 0,
            timestamp: at(1),
            target,
            snapped: TILT,
            site_lat: 35.33,
            site_lon: -97.27,
            image: Some(egui::ColorImage::from_rgba_unmultiplied([2, 2], &[7u8; 16])),
            max_range_km: 230.0,
            nyquist_ms: None,
            melting_layer_source: None,
            storm_motion: None,
            polar: geometry_over_one_gate(),
            codes: None,
        })
        .expect("the receiver lives on the App");
    app.poll_loop_render_results(&ctx);

    let picture = frame_picture(&app, 0).expect("the render landed on the frame");
    let texture = picture
        .surface
        .raster()
        .map(egui::TextureHandle::id)
        .expect("the raster arm uploaded a texture");
    let hover = Arc::clone(&picture.hover);
    assert_eq!(
        hover.read(0.0, 0.25),
        squallar_radar::hover::Reading::Value(GATE_VALUE),
        "fixture: the readout reads nothing even with the volume resident, so \
         nothing below can show that it survives the drop",
    );
    assert!(
        app.loop_mgr.get_cached(SITE, &at(1)).is_some() && app.loop_mgr.cached_scan_bytes() > 0,
        "precondition: the volume the picture was drawn from is in the cache",
    );
    assert!(
        !app.loop_mgr.has_archive(SITE, &at(1)),
        "fixture: an archive behind the volume makes this the ordinary \
         eviction, not the ruling's case",
    );
    drain_uploads(&ctx);

    // The drop. `retain_scans` and not `evict_decoded_except`, because the
    // latter refuses an archive-less volume — refusing it is the very
    // behaviour the ruling is about — and this is the mechanism a ruling
    // would extend to the residency pass.
    let dropped = app.loop_mgr.retain_scans(|_, _, _| false);

    assert_eq!(dropped.len(), 1, "the volume left the cache");
    assert_eq!(
        app.loop_mgr.cached_scan_bytes(),
        0,
        "the cache gave the bytes back",
    );
    drop(dropped);
    let after = frame_picture(&app, 0).expect(
        "the frame lost its picture when the volume behind it went: a loop \
         playing over this frame would show a gap",
    );
    assert_eq!(
        after.surface.raster().map(egui::TextureHandle::id),
        Some(texture),
        "the frame is holding a different texture than the one it was drawn with",
    );
    assert!(
        ctx.tex_manager().read().meta(texture).is_some(),
        "egui no longer holds the frame's texture, so there is nothing on the \
         glass however the frame reads",
    );
    assert!(
        drain_uploads(&ctx).is_empty(),
        "the frame was re-uploaded, so the picture did not survive — it was rebuilt",
    );
    assert_eq!(
        after.hover.read(0.0, 0.25),
        squallar_radar::hover::Reading::Value(GATE_VALUE),
        "the readout lost its number with the volume",
    );
    assert!(
        after.hover.pinned_volume_bytes() > 0,
        "the frame's gates are gone, so the readout above is answering out of \
         a field rather than out of the sweep this asserts about",
    );
}

/// **A textured frame's archive-less volume is what the level counts**, with
/// its four denominators — and the census is asked with the residency pass's
/// own predicate, after that pass has run, so an entry it calls unwanted is
/// one a guard refused.
///
/// TAMPER: drop the `has_archive` guard in `App::loop_decoded_census` and the
/// control below reddens; drop the `wanted(..)` test and the parked frame
/// joins the count.
#[test]
fn an_archive_less_frame_the_policy_has_stopped_wanting_is_counted_and_priced() {
    let ctx = egui::Context::default();
    let mut app = looping_pane(&[at(1), at(2), at(3), at(4), at(5)]);
    let price = app
        .loop_mgr
        .cached_scan_price(SITE, &at(1))
        .expect("the cache priced the volume it filed");
    // Textured, and the playhead is on the other frame: `decoded_wanted` keeps
    // a frame that has no texture yet or is inside the decoded lookahead, and
    // this is neither.
    let ls = app
        .gui
        .pane_mut(0)
        .expect("a headless app has a pane")
        .time_state_mut(&known::RADAR);
    ls.frames[0].image = Some(textured(&ctx));
    // **Four frames ahead of the playhead**, and the desktop decoded lookahead
    // is two: a textured frame inside the lookahead is one `decoded_wanted`
    // still keeps, and asserting over it would assert nothing.
    ls.settle_playhead(squallar_egui::pane::TimeMode::AsOf(at(2)));
    assert_eq!(ls.current_frame(), 1, "fixture: the playhead is on frame 1");

    app.evict_unneeded_loop_scans();

    let census = app.loop_decoded;
    assert_eq!(
        (census.volumes, census.no_archive),
        (5, 5),
        "both fixture volumes are in the cache and neither has an archive",
    );
    assert_eq!(
        (census.unwanted, census.unwanted_bytes),
        (1, price),
        "the textured frame outside the lookahead is the one the policy has \
         stopped wanting, and it is priced at what the cache filed it at",
    );
    assert_eq!(
        (census.sole, census.sole_bytes),
        (1, price),
        "nothing else in this app names that allocation, so its bytes are \
         what a drop could bank",
    );
    assert!(
        census.oldest_unwanted_s > 0,
        "the exposure window reads zero for a volume flown in the past",
    );
    assert!(
        app.loop_mgr.get_cached(SITE, &at(1)).is_some(),
        "control: the eviction pass took the volume after all, so the level \
         above is describing something other than what a guard refused",
    );
}

/// **The split the ruling turns on**: two volumes with no way back, one that
/// never had one and one whose way back the archive ceiling took.
///
/// Both are unwanted and both are un-evictable, and they cost different
/// things to drop — the first is data nobody can re-obtain until the bucket
/// has it, the second is a download. A level that reported only `unwanted`
/// would price them the same.
///
/// TAMPER: answer `ever_had_archive` with a constant `false` and
/// `never_archived` reads 2; drop the `archives_ever` insert in
/// `cache_archive` and it reads 2 as well.
#[test]
fn a_volume_that_lost_its_archive_is_unwanted_but_not_a_fidelity_cost() {
    let ctx = egui::Context::default();
    let mut app = looping_pane(&[at(1), at(2), at(3), at(4), at(5)]);
    let never = app
        .loop_mgr
        .cached_scan_price(SITE, &at(1))
        .expect("the cache priced the volume it filed");
    let lost = app
        .loop_mgr
        .cached_scan_price(SITE, &at(5))
        .expect("the cache priced the volume it filed");
    // The second volume had a way back, and the archive ceiling took it —
    // spelled as the retention pass dropping every archive, which is the same
    // state and needs no ceiling a fixture cannot reach.
    app.loop_mgr
        .cache_archive(SITE, at(5), Arc::new(vec![0u8; 64]));
    app.loop_mgr.retain_archives(|_, _, _| false);
    assert!(
        !app.loop_mgr.has_archive(SITE, &at(5)),
        "fixture: the archive is still held, so this is the ordinary trade",
    );
    let ls = app
        .gui
        .pane_mut(0)
        .expect("a headless app has a pane")
        .time_state_mut(&known::RADAR);
    ls.frames[0].image = Some(textured(&ctx));
    ls.frames[4].image = Some(textured(&ctx));
    ls.settle_playhead(squallar_egui::pane::TimeMode::AsOf(at(2)));

    app.evict_unneeded_loop_scans();

    let census = app.loop_decoded;
    assert_eq!(
        (census.unwanted, census.unwanted_bytes),
        (2, never + lost),
        "both volumes are unwanted and neither can be evicted",
    );
    assert_eq!(
        (census.never_archived, census.never_archived_bytes),
        (1, never),
        "only the volume no archive was ever filed for costs fidelity; the \
         other is a download",
    );
}

/// **The control**: the same frame with an archive behind it is evicted by the
/// pass, and the level says so by counting nothing. Without this the test
/// above passes on a census that counts every unwanted frame whatever its
/// archive.
#[test]
fn the_same_frame_with_an_archive_behind_it_is_evicted_and_counted_nowhere() {
    let ctx = egui::Context::default();
    let mut app = looping_pane(&[at(1), at(2), at(3), at(4), at(5)]);
    app.loop_mgr
        .cache_archive(SITE, at(1), Arc::new(vec![0u8; 64]));
    // **And one archive behind a frame the pass KEEPS** — the playhead's own,
    // which is untextured and so wanted. It is what makes `no_archive` a
    // gated figure rather than a restatement of "what survived": without the
    // census's archive test this volume joins the count.
    app.loop_mgr
        .cache_archive(SITE, at(2), Arc::new(vec![0u8; 64]));
    let ls = app
        .gui
        .pane_mut(0)
        .expect("a headless app has a pane")
        .time_state_mut(&known::RADAR);
    ls.frames[0].image = Some(textured(&ctx));
    // **Four frames ahead of the playhead**, and the desktop decoded lookahead
    // is two: a textured frame inside the lookahead is one `decoded_wanted`
    // still keeps, and asserting over it would assert nothing.
    ls.settle_playhead(squallar_egui::pane::TimeMode::AsOf(at(2)));
    assert_eq!(ls.current_frame(), 1, "fixture: the playhead is on frame 1");

    app.evict_unneeded_loop_scans();

    assert!(
        app.loop_mgr.get_cached(SITE, &at(1)).is_none(),
        "premise: a volume with a way back is evicted by the residency pass",
    );
    let census = app.loop_decoded;
    assert_eq!(
        (census.volumes, census.no_archive),
        (4, 3),
        "the evicted frame is gone, and the surviving frame that has an \
         archive is not one of the archive-less ones",
    );
    assert_eq!(
        (census.unwanted, census.unwanted_bytes, census.sole),
        (0, 0, 0),
        "nothing the residency pass has stopped wanting is still resident",
    );
}

/// **`pinned`: the floor under `LOOP_DECODED_CEILING_BYTES`**, and the column
/// the control above is named `counted_nowhere` for.
///
/// That name was accurate and it was the gap. A volume WITH its archive behind
/// it that `evict_decoded_to_ceiling` may still never take — because a pane is
/// parked on it, or its site is settling — landed in `volumes` and `bytes` and
/// in no other column, so the census could say what the archive-less GUARD
/// refused and nothing at all about what the CEILING could not reach. Those are
/// different sets and only the second one bounds a lowering: the ceiling
/// reclaims in the room above this figure, so a ceiling set under it reclaims
/// nothing however far it falls.
///
/// Why that mattered enough to add a column. `loop scans` is the largest
/// family on the six-pane arm, its ceiling is the obvious thing to lower, and
/// this campaign has already banked a ~94 MiB cut that executed zero times
/// because its precondition never held on the measured arm and no counter said
/// so. `pinned` is this ceiling's precondition, as a number, on the row a leg
/// scrapes.
///
/// The fixture is the control's, deliberately: same app, same archives, same
/// playhead, so the two tests differ only in which column they read.
///
/// TAMPER: drop the `pinned(..)` test in `App::loop_decoded_census` and this
/// reads 0; drop its `entry.has_archive &&` and the archive-less frames join
/// the count, breaking the disjointness this also asserts.
#[test]
fn a_volume_the_ceiling_may_never_take_is_counted_as_the_floor_under_it() {
    let ctx = egui::Context::default();
    let mut app = looping_pane(&[at(1), at(2), at(3), at(4), at(5)]);
    // **Exactly one way back, behind the volume the pane is parked on**, and
    // the narrowing is deliberate. Every fixture volume here carries the SAME
    // collected instant, so the pin predicate's second clock
    // (`Some(at) == collected`) matches all five; the archive is what singles
    // one out, and it is also the column's own premise — an entry with no way
    // back is refused by the guard and belongs to `no_archive`, never here.
    app.loop_mgr
        .cache_archive(SITE, at(2), Arc::new(vec![0u8; 64]));
    let parked_price = app
        .loop_mgr
        .cached_scan_price(SITE, &at(2))
        .expect("the cache priced the volume it filed");
    // **What actually pins a volume against the ceiling**, and the fixture
    // detail worth stating because getting it wrong makes this test unable to
    // reach the state it asserts: the pass's `pinned` closure reads `parked`,
    // which `evict_unneeded_loop_scans` builds from each pane's `scan_info` —
    // the volume the pane is DISPLAYING — and not from a loop's playhead. A
    // headless pane has no `scan_info` until something sets one, so without
    // this the column reads 0 and the test would be asserting the absence of
    // the very thing it is for.
    let displayed = app
        .loop_mgr
        .get_cached(SITE, &at(2))
        .expect("fixture: the cache holds the volume the pane is parked on")
        .0
        .clone();
    let pane = app.gui.pane_mut(0).expect("a headless app has a pane");
    pane.scan_info = Some(squallar_radar::types::ScanInfo::from_scan(
        &displayed,
        SITE,
        at(2),
        None,
    ));
    let ls = pane.time_state_mut(&known::RADAR);
    ls.frames[0].image = Some(textured(&ctx));
    ls.settle_playhead(squallar_egui::pane::TimeMode::AsOf(at(2)));
    assert_eq!(ls.current_frame(), 1, "fixture: the playhead is on frame 1");

    app.evict_unneeded_loop_scans();

    let census = app.loop_decoded;
    assert_eq!(
        (census.pinned, census.pinned_bytes),
        (1, parked_price),
        "the volume a pane is parked on has its archive behind it and is the \
         one the ceiling may never take, priced at what the cache filed it at",
    );
    assert!(
        app.loop_mgr.get_cached(SITE, &at(2)).is_some(),
        "control: the parked volume was evicted after all, so the column \
         above is describing something other than what the ceiling cannot \
         reach",
    );
    // **Disjoint from `no_archive` by construction**, which is what lets a
    // reader ADD the two into one floor. Overlapping columns would double
    // count the entries that are both, and the sum would rank a lowering
    // against a figure larger than the cache.
    assert_eq!(
        (census.volumes, census.no_archive),
        (5, 4),
        "five volumes resident, and the four with no way back are the \
         guard's, not the ceiling's",
    );
    assert_eq!(
        census.pinned + census.no_archive,
        census.volumes,
        "the two refusals partition this cache exactly: every resident volume \
         is refused either for having no way back or for being pinned, and \
         none is counted twice",
    );
    assert!(
        census.pinned + census.no_archive <= census.volumes,
        "the two refusals must partition rather than overlap",
    );
}

/// **`0f3de05db`'s lesson, applied to this cache**: an unwanted volume another
/// store also names frees nothing when it goes, and a level without a `sole`
/// column would report it as a prize.
///
/// TAMPER: delete the `elsewhere` test in `App::loop_decoded_census` and this
/// reads `sole 1`.
#[test]
fn an_unwanted_volume_the_still_store_also_names_is_counted_but_not_sole() {
    let ctx = egui::Context::default();
    let mut app = looping_pane(&[at(1), at(2), at(3), at(4), at(5)]);
    let shared = Arc::clone(
        &app.loop_mgr
            .get_cached(SITE, &at(1))
            .expect("the fixture filed the volume")
            .0,
    );
    let price = app
        .loop_mgr
        .cached_scan_price(SITE, &at(1))
        .expect("the cache priced the volume it filed");
    let info = app
        .gui
        .pane(0)
        .expect("a headless app has a pane")
        .scan_info
        .clone()
        .expect("point_at_site put scan info on the pane");
    app.latest_cached_scans
        .insert(SITE.to_string(), (shared, Default::default(), info, at(1)));
    let ls = app
        .gui
        .pane_mut(0)
        .expect("a headless app has a pane")
        .time_state_mut(&known::RADAR);
    ls.frames[0].image = Some(textured(&ctx));
    // **Four frames ahead of the playhead**, and the desktop decoded lookahead
    // is two: a textured frame inside the lookahead is one `decoded_wanted`
    // still keeps, and asserting over it would assert nothing.
    ls.settle_playhead(squallar_egui::pane::TimeMode::AsOf(at(2)));
    assert_eq!(ls.current_frame(), 1, "fixture: the playhead is on frame 1");

    app.evict_unneeded_loop_scans();

    let census = app.loop_decoded;
    assert_eq!(
        (census.unwanted, census.unwanted_bytes),
        (1, price),
        "the frame is still one the policy has stopped wanting",
    );
    assert_eq!(
        (census.sole, census.sole_bytes),
        (0, 0),
        "the per-site latest names the same allocation, so dropping the loop \
         cache's reference would not move a byte",
    );
}

/// **A textured frame two ahead of the playhead keeps a whole decoded volume
/// that nothing reads.**
///
/// `LOOP_DECODED_LOOKAHEAD_FRAMES` is retarget insurance and nothing else:
/// `App::evict_unneeded_loop_scans`' own comment says only a first render and
/// a retarget re-render read a decoded volume, and "playback reads neither".
/// So on a loop whose frames are all textured, every volume the lookahead
/// holds is idle — and the desktop arm held two of them per site.
///
/// The size of that insurance is now measured rather than assumed. Over 39
/// Archive II volumes, release build, x86_64 Linux, best of three, one volume
/// at a time: `scan::decode_shared` costs 6.0 / 10.7 / 40.9 ms
/// (min / median / max) and the volume it rebuilds is 30.6 / 45.0 / 74.8 MiB.
/// The constant's own doc gives the rule — `ceil(decode_latency /
/// frame_interval)` — and at `DEFAULT_LOOP_SPEED_FPS` (5 fps, a 200 ms frame)
/// that is `ceil(40.9 / 200) = 1` on the WORST volume in the corpus, not 2.
///
/// This asserts the frame at forward distance 2, which is the one the second
/// frame of lookahead was buying.
#[test]
fn a_textured_frame_two_ahead_of_the_playhead_does_not_keep_its_moments() {
    let ctx = egui::Context::default();
    let mut app = looping_pane(&[at(1), at(2), at(3), at(4), at(5)]);
    // A way back, so the residency pass is allowed to consider the entry at
    // all: `evict_decoded_except` refuses a volume with no archive behind it
    // whatever the predicate says.
    app.loop_mgr
        .cache_archive(SITE, at(4), Arc::new(vec![0u8; 64]));
    let ls = app
        .gui
        .pane_mut(0)
        .expect("a headless app has a pane")
        .time_state_mut(&known::RADAR);
    // Textured, so `decoded_wanted`'s `frame.image.is_none()` arm cannot be
    // what keeps it — the lookahead is then the only thing that can.
    ls.frames[3].image = Some(textured(&ctx));
    ls.settle_playhead(squallar_egui::pane::TimeMode::AsOf(at(2)));
    assert_eq!(ls.current_frame(), 1, "fixture: the playhead is on frame 1");
    // (idx + (total - playhead)) % total = (3 + 4) % 5 = 2.
    assert_eq!(
        (3 + (5 - ls.current_frame())) % 5,
        2,
        "fixture: frame 3 is two ahead of the playhead",
    );

    app.evict_unneeded_loop_scans();

    assert!(
        app.loop_mgr.get_cached(SITE, &at(4)).is_none(),
        "a textured frame two ahead of the playhead, with an archive to come \
         back from, still holds its decoded volume. Nothing reads it: \
         playback draws the texture. It is retarget insurance priced at a \
         median 45.0 MiB to save a median 10.7 ms decode.",
    );
}

/// **The fires-counter fires**, on the arm the cut is aimed at.
///
/// `decoded trades:` is a new row rather than a new number on an existing one,
/// so the binary before the cut prints no such row at all and cannot be
/// mistaken for one reading zero.
#[test]
fn the_trade_counter_counts_the_volume_the_lookahead_stopped_wanting() {
    let ctx = egui::Context::default();
    let mut app = looping_pane(&[at(1), at(2), at(3), at(4), at(5)]);
    app.loop_mgr
        .cache_archive(SITE, at(4), Arc::new(vec![0u8; 64]));
    let ls = app
        .gui
        .pane_mut(0)
        .expect("a headless app has a pane")
        .time_state_mut(&known::RADAR);
    ls.frames[3].image = Some(textured(&ctx));
    ls.settle_playhead(squallar_egui::pane::TimeMode::AsOf(at(2)));
    assert_eq!(
        (app.loop_decoded_traded, app.loop_decoded_traded_bytes),
        (0, 0),
        "premise: nothing has been traded before the pass runs",
    );

    app.evict_unneeded_loop_scans();

    assert_eq!(
        app.loop_decoded_traded, 1,
        "the pass traded exactly the one textured frame outside the lookahead",
    );
    assert!(
        app.loop_decoded_traded_bytes > 0,
        "a traded volume weighed nothing: the byte arm of the counter is \
         reading a total the cache never moved. Counts and bytes are \
         different currencies and a live count does not vouch for the bytes.",
    );
}

/// **And it stays silent where the trade is UNAVAILABLE** — the direction that
/// matters, because this is the shape that let a ~94 MiB cut on this campaign
/// deliver exactly zero.
///
/// Identical to the test above but for the archive. `evict_decoded_except`
/// refuses a volume with no way back whatever the residency rule says, so on a
/// chunk-fed scene a narrower lookahead frees nothing — and the counter has to
/// be able to say so rather than reporting the trade it did not make.
#[test]
fn the_trade_counter_stays_zero_when_there_is_no_archive_to_trade_for() {
    let ctx = egui::Context::default();
    let mut app = looping_pane(&[at(1), at(2), at(3), at(4), at(5)]);
    let ls = app
        .gui
        .pane_mut(0)
        .expect("a headless app has a pane")
        .time_state_mut(&known::RADAR);
    ls.frames[3].image = Some(textured(&ctx));
    ls.settle_playhead(squallar_egui::pane::TimeMode::AsOf(at(2)));

    app.evict_unneeded_loop_scans();

    assert!(
        app.loop_mgr.get_cached(SITE, &at(4)).is_some(),
        "premise: with no archive behind it the volume is not evictable",
    );
    assert_eq!(
        (app.loop_decoded_traded, app.loop_decoded_traded_bytes),
        (0, 0),
        "the counter reported a trade the policy refused to make",
    );
}
