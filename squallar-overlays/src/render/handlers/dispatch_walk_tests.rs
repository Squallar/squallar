//! **How many times a dispatch walks a layer's item list**, counted rather
//! than read off the source — and, beside it, the control that says the walks
//! that remain still produce what they used to.
//!
//! # Why a count, and why beside a content check
//!
//! `spawn_overlay_render` asks each dispatched layer for two things back to
//! back: `prepare_job` and `hit_items`. The `frame dispatch (prepare)` and
//! `frame dispatch (hitmap)` cuts time exactly those two calls. A time is not
//! a count: a cut reading high is equally consistent with one walk of a large
//! list, two walks of a small one, or a thread that was descheduled between
//! the two `Instant::now()` reads and walked nothing at all. So the walks are
//! counted where they happen — [`squallar_source::walks`] — and asserted here.
//!
//! **And a falling count is not on its own a saving.** Halving the traversals
//! is trivially achievable by producing half the output, and a cost figure
//! cannot tell that apart from a real improvement. So every assertion about a
//! count in this file sits next to an assertion about the dispatch's *output*:
//! that the paint rows and the hit items are unchanged from the dispatch
//! before, and that they still line up with each other index for index.
//!
//! The three failure modes and what catches each:
//!
//! | defect | caught by |
//! |---|---|
//! | a walk comes back per dispatch | the count not scaling with dispatches |
//! | fewer items produced | coverage against the handler's own item count |
//! | items shifted against the rows | the intra-dispatch alignment check |

use std::sync::Arc;

use super::sources;
use super::texture_tests::seed;
use crate::render::overlay_state::{OverlayRegistry, PaneRef, RasterizeContext};
use crate::render::rasterize;
use squallar_source::hit::HitItems;
use squallar_source::id::{LayerId, known};
use squallar_source::walks;

/// The two dispatch counts each layer is driven through. **Different on
/// purpose**: a walk that is per-dispatch scales with these and a walk that is
/// per-generation does not, so comparing the two runs distinguishes them
/// without either number being a magic constant to re-point.
const FEW: u64 = 4;
const MANY: u64 = 16;

/// A fixed context, captured once: the memos key on the data generation and a
/// view fold, and re-reading the clock per dispatch would move neither. This
/// is the *steady* posture — the one a pane holds between polls, and the one
/// the overwhelming majority of dispatches in any window are taken in.
fn ctx() -> RasterizeContext {
    let clock = chrono::Utc::now().naive_utc();
    RasterizeContext {
        device_scale: 1.0,
        is_dark: false,
        zoom: 7.0,
        now: clock,
        as_of: clock,
        frame: None,
    }
}

/// The registry with **the two hit-map layers seeded**, and the rest left as
/// they are. `seed` refuses a layer it has no fixture for, and the layers this
/// file measures are exactly the two that answer `hit_items`.
fn registry_seeded() -> OverlayRegistry {
    let mut handlers = sources();
    let mut seeded = 0;
    for handler in handlers.iter_mut() {
        let id = handler.id();
        if id == known::STORM_REPORTS || id == known::LIGHTNING {
            assert!(seed(handler.as_mut()), "{} seeds", id.as_str());
            seeded += 1;
        }
    }
    assert_eq!(seeded, 2, "both hit-map layers must be seeded");
    OverlayRegistry::with_handlers(handlers)
}

/// One dispatch's whole output, flattened so that two dispatches can be
/// compared element-wise, and so that the two halves of a single dispatch can
/// be compared against *each other* at the same index.
///
/// `rows` and `items` are written in the **same key format**, built from the
/// paint row and from the click-resolved item respectively. So
/// `rows[i] == items[i]` states the index-alignment contract directly:
/// `hit_items()[i]` IS the item at paint row `i`. A hit list shifted, rotated
/// or reordered against the rows moves one and not the other — while having
/// the same length and costing the same to build.
///
/// `indices` is each item's own recorded position, which is the id a hit-map
/// cell carries; it must be `0..n` or a click resolves through the wrong row.
#[derive(Debug, PartialEq)]
struct Produced {
    rows: Vec<String>,
    items: Vec<String>,
    indices: Vec<usize>,
}

impl Produced {
    /// **The contract, checked inside one dispatch**: the item at position `i`
    /// is the report the row at position `i` paints, and it carries `i` as its
    /// own recorded index.
    fn assert_aligned(&self, name: &str) {
        assert_eq!(
            self.rows, self.items,
            "{name}: the hit list and the paint rows are not the same items in \
             the same order. Whatever a hover at row i resolves to, it is not \
             what row i drew — and no downstream test can see it, because they \
             all zip with this very list",
        );
        assert!(
            self.indices.iter().copied().eq(0..self.indices.len()),
            "{name}: the items' own recorded indices are {:?}, not 0..{}; a \
             cell's id is that index, so a click resolves through the wrong row",
            self.indices,
            self.indices.len(),
        );
    }
}

fn produced(job: &squallar_source::job::DescribedJob, items: &HitItems) -> Produced {
    let resolved: Vec<Arc<dyn squallar_source::handler::OverlayItem>> = items.iter().collect();

    if let Some(input) = job.downcast_ref::<rasterize::ReportsInput>() {
        let key = |kind: &crate::spc::reports::StormReportKind,
                   lat: f64,
                   lon: f64,
                   valid: &Option<chrono::NaiveDateTime>| {
            format!("{kind:?}|{lat}|{lon}|{valid:?}")
        };
        let mut item_keys = Vec::new();
        let mut indices = Vec::new();
        for item in &resolved {
            let it = item
                .as_any()
                .downcast_ref::<super::reports::StormReportItem>()
                .expect("a reports hit list resolves to report items");
            item_keys.push(key(
                &it.report.kind,
                it.report.lat,
                it.report.lon,
                &it.report.valid,
            ));
            indices.push(it.index);
        }
        return Produced {
            rows: input
                .reports
                .iter()
                .map(|r| key(&r.kind, r.lat, r.lon, &r.valid))
                .collect(),
            items: item_keys,
            indices,
        };
    }

    if let Some(input) = job.downcast_ref::<rasterize::GlmStrikesInput>() {
        let key = |lat: f64, lon: f64, time: &chrono::NaiveDateTime, energy: &Option<f32>| {
            format!("{lat}|{lon}|{time:?}|{energy:?}")
        };
        let mut item_keys = Vec::new();
        let mut indices = Vec::new();
        for item in &resolved {
            let it = item
                .as_any()
                .downcast_ref::<super::glm::GlmFlashItem>()
                .expect("a GLM hit list resolves to flash items");
            item_keys.push(key(
                it.flash.lat,
                it.flash.lon,
                &it.flash.time,
                &it.flash.energy_j(),
            ));
            indices.push(it.index);
        }
        return Produced {
            rows: input
                .flashes
                .iter()
                .map(|f| key(f.lat, f.lon, &f.time, &f.energy))
                .collect(),
            items: item_keys,
            indices,
        };
    }

    panic!("a hit-map layer described another layer's input: {job:?}");
}

/// Drive one layer through `count` dispatches exactly as the dispatch tail
/// does — `prepare_job`, then `hit_items` — and answer what it cost and what
/// it produced.
fn drive(
    registry: &OverlayRegistry,
    id: &LayerId,
    count: u64,
) -> (walks::WalkTotals, Vec<Produced>) {
    let ctx = ctx();
    let pane = PaneRef::bare(0);
    let before = walks::totals();
    let mut outputs = Vec::new();
    for _ in 0..count {
        let job = registry
            .prepare_job(id, &ctx, &pane)
            .expect("a seeded hit-map layer describes a job");
        let items = registry
            .hit_items(id)
            .expect("a seeded hit-map layer answers a hit list");
        outputs.push(produced(&job, &items));
    }
    (walks::totals().since(before), outputs)
}

/// **The measurement, and the property it pins.** At an unmoved generation and
/// view, how many times does each hit-map layer walk its item list per
/// dispatch — and does every dispatch still produce exactly what the first one
/// did, in the same order?
///
/// Denominator: the dispatch counts below, counted by
/// `OverlayRegistry::prepare_job` itself rather than by this loop, so the
/// figure is the dispatch path's own and not this test's idea of it.
///
/// # A property, not a number
///
/// The layer is driven through [`FEW`] dispatches and then [`MANY`] more, and
/// the second run must walk **nothing**. A walk that happens once per dispatch
/// makes the second run scale with `MANY`; a walk that happens once per
/// generation cannot. So this cannot be satisfied by re-pointing a constant
/// when a per-dispatch walk comes back — which is how the walk this file was
/// written for got in.
///
/// # Recorded figures
///
/// Baseline, before the storm-report hit list became a slab handle, over 8
/// dispatches of each layer at one generation and one view:
///
/// | layer | dispatches | paint walks | hit walks | walks/dispatch |
/// |---|---|---|---|---|
/// | storm reports | 8 | 1 | 8 | 1.125 |
/// | GLM lightning | 8 | 1 | 0 | 0.125 |
///
/// The eight were the storm-report layer materialising one `Arc` per row on
/// every dispatch. **The paint side was already once per poll on both** — the
/// "two walks back to back per dispatch" this file was written to find had
/// already been half-shed by the built-input memo and half-shed by the GLM
/// slab, and it is the count that showed it rather than a reading of the
/// source.
#[test]
fn neither_half_of_a_dispatch_walks_the_item_list_per_dispatch() {
    let registry = registry_seeded();

    for (id, name) in [
        (known::STORM_REPORTS, "storm reports"),
        (known::LIGHTNING, "GLM lightning"),
    ] {
        let (few, few_out) = drive(&registry, &id, FEW);
        let (many, many_out) = drive(&registry, &id, MANY);

        assert_eq!(few.dispatches, FEW, "{name}: the registry's own count");
        assert_eq!(many.dispatches, MANY, "{name}: the registry's own count");

        // ---- the cost half: a walk must not scale with dispatches ---------
        assert_eq!(
            (many.paint_walks, many.hit_walks),
            (0, 0),
            "{name}: {MANY} further dispatches at the same generation and view \
             walked the item list again ({} paint, {} hit). Everything either \
             half builds is a function of the generation alone, so a second \
             ask must hand back what the first built. A walk here is a whole \
             item list traversed on the frame thread, per dispatch, per pane.",
            many.paint_walks,
            many.hit_walks,
        );
        assert!(
            few.walks() <= 2,
            "{name}: the first {FEW} dispatches walked {} times; at most one \
             build per half per generation is the contract",
            few.walks(),
        );

        // ---- the work-held half -------------------------------------------
        // A count that falls because less is produced is not a saving.
        let first = &few_out[0];
        assert!(
            !first.rows.is_empty() && !first.items.is_empty(),
            "{name}: the fixture produced nothing, so every equality below \
             would hold vacuously",
        );
        // Within one dispatch: the two halves agree index for index. This is
        // what a shift or a rotation moves, and it is invisible both to the
        // count above and to any length check.
        first.assert_aligned(name);
        // Across dispatches: cheaper must also mean identical.
        for (n, later) in few_out.iter().chain(many_out.iter()).enumerate() {
            assert_eq!(
                later, first,
                "{name}: dispatch {n} produced different output from dispatch \
                 0 at an unmoved generation and view. A memo or a slab that \
                 makes a dispatch cheaper must make it cheaper and identical, \
                 not cheaper and smaller",
            );
        }

        eprintln!(
            "walk count [{name}]: first {FEW} dispatches paint={} hit={}; next \
             {MANY} dispatches paint={} hit={} -> {:.3} walks/dispatch over {} \
             dispatches; rows={} items={}",
            few.paint_walks,
            few.hit_walks,
            many.paint_walks,
            many.hit_walks,
            (few.walks() + many.walks()) as f64 / (FEW + MANY) as f64,
            FEW + MANY,
            first.rows.len(),
            first.items.len(),
        );
    }
}

/// **Both halves cover the layer's own item count** — an independent source,
/// not the other derived vector.
///
/// `a_hit_map_kinds_items_align_with_its_described_rows` checks the rows and
/// the items against *each other*, which two vectors built from one truncated
/// walk would satisfy together. This checks both against the count the handler
/// reports for its own data, which no walk of that data produces.
#[test]
fn both_halves_cover_the_layers_own_item_count() {
    let mut handlers = sources();
    let mut checked = 0;
    for handler in handlers.iter_mut() {
        let id = handler.id();
        if id != known::STORM_REPORTS && id != known::LIGHTNING {
            continue;
        }
        assert!(seed(handler.as_mut()), "{} seeds", id.as_str());
        let pane = PaneRef::bare(0);
        let held = handler.item_count(&pane);
        assert!(held > 0, "{}: the fixture seeded nothing", id.as_str());

        let job = handler.prepare_job(&ctx(), &pane).expect("seeded");
        let items = handler.hit_items().expect("seeded");
        let out = produced(&job, &items);
        out.assert_aligned(id.as_str());
        assert_eq!(
            out.rows.len(),
            held,
            "{}: the paint rows do not cover the layer's {held} items",
            id.as_str(),
        );
        assert_eq!(
            out.items.len(),
            held,
            "{}: the hit list does not cover the layer's {held} items",
            id.as_str(),
        );
        checked += 1;
    }
    assert_eq!(checked, 2, "both hit-map kinds must be walked seeded");
}
