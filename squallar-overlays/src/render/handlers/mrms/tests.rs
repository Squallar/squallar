use super::*;
use crate::mrms::{DESKTOP_GRID_HISTORY_ENTRIES, WASM_GRID_HISTORY_ENTRIES};
use crate::render::gridded::ResidentGrid;
use squallar_geo::GeoBounds;

const BOUNDS: GeoBounds = GeoBounds {
    min_lat: 34.0,
    max_lat: 36.0,
    min_lon: -99.0,
    max_lon: -97.0,
};

/// A small mosaic over [`BOUNDS`], `points` values wide by one row, so a test
/// can size a grid against a budget it can overflow.
fn grid_of(product: MrmsProduct, values: Vec<f32>) -> MrmsGrid {
    grid_of_values(product, crate::render::gridded::GridValues::F32(values))
}

/// [`grid_of`] over a store of either width, so a fixture can be built on the
/// arm the thing under test actually needs.
fn grid_of_values(product: MrmsProduct, values: crate::render::gridded::GridValues) -> MrmsGrid {
    let ni = values.len().max(2);
    let spec = crate::mrms::fields::spec(product);
    let paint = crate::render::gridded::field_paint(&spec.id).expect("registered");
    let (visible_points, value_range) = values.summarize(|v| paint.paints(v));
    MrmsGrid {
        product,
        grid: Arc::new(ResidentGrid {
            field: spec.id.clone(),
            ni,
            nj: 1,
            coords: crate::hrrr::GridCoords::Regular {
                lat0: BOUNDS.max_lat,
                lon0: BOUNDS.min_lon,
                dlat: -0.01,
                dlon: (BOUNDS.max_lon - BOUNDS.min_lon) / (ni - 1) as f64,
                ni,
                nj: 1,
                scan_mode: 0,
            },
            values,
        }),
        bounds: BOUNDS,
        valid: chrono::NaiveDate::from_ymd_opt(2026, 8, 21)
            .unwrap()
            .and_hms_opt(0, 0, 39)
            .unwrap(),
        visible_points,
        value_range,
    }
}

/// A mosaic of exactly `n` values, all drawable. These fixtures are built on
/// the **wide** arm, so `resident_bytes()` is `4n` here; a real decoded mosaic
/// is on the narrow arm and is `2n`.
fn sized(product: MrmsProduct, n: usize) -> MrmsGrid {
    grid_of(product, vec![45.0; n])
}

fn handler_with(product: MrmsProduct, values: Vec<f32>) -> MrmsHandler {
    let mut h = MrmsHandler::new();
    h.defaults.enabled = true;
    h.defaults.selected_product = product;
    h.apply_fetch_result(
        Box::new(MrmsFetchResult(Ok(grid_of(product, values)))),
        &PaneRef::across(&[]),
    );
    h
}

fn pane_state(product: MrmsProduct) -> Box<MrmsPaneState> {
    Box::new(MrmsPaneState {
        enabled: true,
        selected_product: product,
    })
}

// ── The byte budget ─────────────────────────────────────────────────────────

/// A cache with its history at the value that never binds, so the byte-budget
/// tests below exercise the ceiling alone. [`DESKTOP_GRID_HISTORY_ENTRIES`] is
/// that value by construction: one short of the key space, and the arrival is
/// never counted against the history.
fn byte_budget_cache(budget: usize) -> MrmsGridCache {
    MrmsGridCache::new(budget, DESKTOP_GRID_HISTORY_ENTRIES, staging::global())
}

/// **The instrument that makes every eviction test below non-vacuous.** A cache
/// whose budget it cannot overflow proves nothing about eviction.
#[test]
fn the_budget_is_spent_in_bytes_and_a_grid_that_fits_is_kept() {
    // Room for exactly one 100-value grid (400 bytes).
    let mut cache = byte_budget_cache(400);
    let a = Arc::new(sized(MrmsProduct::ReflectivityComposite, 100));
    assert_eq!(a.resident_bytes(), 400);
    cache.insert(MrmsProduct::ReflectivityComposite, a, &[]);
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.resident_bytes(), 400);
}

/// A second grid that does not fit evicts the first — **by bytes**, with no
/// entry count anywhere in the decision.
#[test]
fn a_grid_that_does_not_fit_evicts_the_least_recently_used_one() {
    let mut cache = byte_budget_cache(400);
    cache.insert(
        MrmsProduct::ReflectivityComposite,
        Arc::new(sized(MrmsProduct::ReflectivityComposite, 100)),
        &[],
    );
    cache.insert(
        MrmsProduct::PrecipRate,
        Arc::new(sized(MrmsProduct::PrecipRate, 100)),
        &[],
    );
    assert_eq!(cache.len(), 1, "the budget holds one grid, not two entries");
    assert!(cache.contains(MrmsProduct::PrecipRate), "the arrival stays");
    assert!(!cache.contains(MrmsProduct::ReflectivityComposite));
    assert!(cache.resident_bytes() <= 400);
}

/// The same two grids fit when the budget doubles — so the eviction above was
/// the *budget*, not a hidden entry cap.
#[test]
fn a_wider_budget_holds_both_grids() {
    let mut cache = byte_budget_cache(800);
    cache.insert(
        MrmsProduct::ReflectivityComposite,
        Arc::new(sized(MrmsProduct::ReflectivityComposite, 100)),
        &[],
    );
    cache.insert(
        MrmsProduct::PrecipRate,
        Arc::new(sized(MrmsProduct::PrecipRate, 100)),
        &[],
    );
    assert_eq!(cache.len(), 2);
    assert_eq!(cache.resident_bytes(), 800);
}

/// **A pinned product is never evicted**, even when that puts the cache over
/// budget: dropping what a pane is showing blanks it with nothing that will
/// re-ask, which is worse than the memory.
#[test]
fn the_cache_never_evicts_a_pinned_product() {
    let mut cache = byte_budget_cache(400);
    cache.insert(
        MrmsProduct::ReflectivityComposite,
        Arc::new(sized(MrmsProduct::ReflectivityComposite, 100)),
        &[],
    );
    cache.insert(
        MrmsProduct::PrecipRate,
        Arc::new(sized(MrmsProduct::PrecipRate, 100)),
        &[MrmsProduct::ReflectivityComposite],
    );
    assert!(
        cache.contains(MrmsProduct::ReflectivityComposite),
        "a pane's own product was evicted to make room for another pane's",
    );
    assert!(cache.contains(MrmsProduct::PrecipRate));
    assert!(
        cache.resident_bytes() > 400,
        "non-triviality: this case really is over budget, so the pin is what \
         kept the entry rather than there being room for it",
    );
}

/// A refetch replaces its own key rather than growing the cache.
#[test]
fn a_refetch_of_a_resident_product_replaces_its_own_key() {
    let mut cache = byte_budget_cache(400);
    let p = MrmsProduct::ReflectivityComposite;
    cache.insert(p, Arc::new(sized(p, 100)), &[]);
    cache.insert(p, Arc::new(sized(p, 100)), &[]);
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.resident_bytes(), 400);
}

/// The shipped budget really does admit a real mosaic, which is the property
/// the injected budget above cannot check.
#[test]
fn the_shipped_budget_admits_a_full_conus_grid() {
    assert!(MrmsHandler::new().cached_grids.budget >= crate::mrms::CONUS_GRID_BYTES);
}

// ── The history budget ──────────────────────────────────────────────────────

/// Room for both products many times over, so nothing in this section is the
/// byte ceiling's doing.
const WIDE: usize = 8_000;

fn wide_cache(history: usize) -> MrmsGridCache {
    MrmsGridCache::new(WIDE, history, staging::global())
}

/// One pane cycles both products. On the wasm arm (history 0) the product it
/// left is gone and only the pinned one is resident; on the desktop arm both
/// stay. Same fixtures, same ceiling — the history is the only difference.
#[test]
fn a_single_pane_that_cycles_products_keeps_what_the_arm_history_says() {
    let (a, b) = (MrmsProduct::ReflectivityComposite, MrmsProduct::PrecipRate);

    let mut wasm = wide_cache(WASM_GRID_HISTORY_ENTRIES);
    wasm.insert(a, Arc::new(sized(a, 100)), &[a]);
    wasm.insert(b, Arc::new(sized(b, 100)), &[b]);
    assert!(wasm.contains(b), "the pinned product is resident");
    assert!(
        !wasm.contains(a),
        "the product the pane left is not: a history of 0 keeps nothing warm",
    );
    assert_eq!(wasm.len(), 1);

    let mut desktop = wide_cache(DESKTOP_GRID_HISTORY_ENTRIES);
    desktop.insert(a, Arc::new(sized(a, 100)), &[a]);
    desktop.insert(b, Arc::new(sized(b, 100)), &[b]);
    assert_eq!(desktop.len(), 2, "the desktop arm keeps every product warm");
    assert!(
        desktop.resident_bytes() <= WIDE,
        "non-triviality: the wasm eviction above was not the byte ceiling's",
    );
}

/// Two panes on two products with a history of 0: both stay. The pin set is the
/// floor of what is resident, and the history is only what may stay beyond it.
#[test]
fn two_panes_on_two_products_keep_both_under_a_zero_history() {
    let (a, b) = (MrmsProduct::ReflectivityComposite, MrmsProduct::PrecipRate);
    let mut cache = wide_cache(0);
    cache.insert(a, Arc::new(sized(a, 100)), &[a, b]);
    cache.insert(b, Arc::new(sized(b, 100)), &[a, b]);
    assert_eq!(cache.len(), 2);
    assert!(cache.contains(a) && cache.contains(b));
}

/// Lowering the history evicts the excess at once — not on the next arrival,
/// which is a two-minute poll away — and raising it evicts nothing. Two
/// products hold one unpinned grid at most, so least-recent-first order among
/// several is the GMGSI suite's to show; this one shows the trim is immediate.
#[test]
fn lowering_the_history_evicts_the_excess_at_once_and_raising_evicts_nothing() {
    let (a, b) = (MrmsProduct::ReflectivityComposite, MrmsProduct::PrecipRate);
    let mut cache = wide_cache(DESKTOP_GRID_HISTORY_ENTRIES);
    cache.insert(a, Arc::new(sized(a, 100)), &[b]);
    cache.insert(b, Arc::new(sized(b, 100)), &[b]);
    assert_eq!(cache.len(), 2, "one pinned, one warm");

    cache.set_history(0, &[b]);
    assert_eq!(cache.history(), 0);
    assert_eq!(cache.len(), 1);
    assert!(cache.contains(b), "the pinned product stays");
    assert!(
        !cache.contains(a),
        "the warm one is the excess and goes now"
    );

    cache.set_history(DESKTOP_GRID_HISTORY_ENTRIES, &[b]);
    assert_eq!(cache.history(), DESKTOP_GRID_HISTORY_ENTRIES);
    assert_eq!(
        cache.len(),
        1,
        "raising the history restores capacity, not grids",
    );
}

/// Every product pinned, a history of 0 and a ceiling one grid wide: both stay,
/// the loop takes its `break` arm without panicking, and the bytes are over the
/// ceiling — the pin rule holds against both budgets at once.
#[test]
fn the_pin_rule_holds_against_both_budgets_at_once() {
    let (a, b) = (MrmsProduct::ReflectivityComposite, MrmsProduct::PrecipRate);
    let mut cache = MrmsGridCache::new(400, 0, staging::global());
    cache.insert(a, Arc::new(sized(a, 100)), &[a, b]);
    cache.insert(b, Arc::new(sized(b, 100)), &[a, b]);
    assert_eq!(cache.len(), 2);
    assert!(cache.contains(a) && cache.contains(b));
    assert!(
        cache.resident_bytes() > 400,
        "non-triviality: this really is over the byte ceiling, so the break arm \
         was taken rather than there having been room",
    );
}

/// An arrival no pane is showing — the pane switched products while the fetch
/// was in flight — is held for its own insert and counted on the next.
#[test]
fn an_unpinned_arrival_is_held_for_its_own_insert_and_counted_on_the_next() {
    let (a, b) = (MrmsProduct::ReflectivityComposite, MrmsProduct::PrecipRate);
    let mut cache = wide_cache(0);
    cache.insert(a, Arc::new(sized(a, 100)), &[b]);
    assert!(cache.contains(a), "the arrival is never its own victim");
    cache.insert(b, Arc::new(sized(b, 100)), &[b]);
    assert!(
        !cache.contains(a),
        "counted on the next insert, it is the excess",
    );
    assert!(cache.contains(b));
}

/// The shipped handler opens at the arm's history — the constant is wired, not
/// merely declared.
#[test]
fn the_shipped_handler_opens_at_the_arm_history() {
    assert_eq!(
        MrmsHandler::new().cached_grids.history(),
        crate::mrms::GRID_HISTORY_ENTRIES,
    );
}

// ── The raster carry ────────────────────────────────────────────────────────

/// `prepare_job` hands over an **`Arc` clone**, not a copy: a 49 MB memcpy per
/// described job would be one per pane per gesture-settle.
#[test]
fn prepare_job_describes_the_resident_grid_without_copying_it() {
    let h = handler_with(MrmsProduct::ReflectivityComposite, vec![45.0; 8]);
    let resident = Arc::clone(
        h.cached_grids
            .get(MrmsProduct::ReflectivityComposite)
            .expect("seeded"),
    );
    let job = h
        .prepare_job(&rctx(), &PaneRef::bare(0))
        .expect("a resident product describes a job");
    let input = job
        .downcast_ref::<rasterize::GriddedInput>()
        .expect("MRMS describes a GriddedInput, which is what lets it share the row");
    let rasterize::GriddedInput::Resident(carried) = input else {
        panic!("MRMS must describe the Resident arm, not {input:?}");
    };
    assert!(
        Arc::ptr_eq(carried, &resident.grid),
        "the described job carries a different allocation than the cache holds, \
         so 49 MB was copied to describe it",
    );
}

/// A pane with no resident mosaic describes nothing — the miss costs a picture,
/// not a wrong one.
#[test]
fn a_pane_with_no_resident_mosaic_describes_no_job() {
    let h = MrmsHandler::new();
    assert!(h.prepare_job(&rctx(), &PaneRef::bare(0)).is_none());
}

fn rctx() -> RasterizeContext {
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

// ── Identity and registration ───────────────────────────────────────────────

/// The layer answers "which field is this pane showing" from its **own per-pane
/// state**, with an id its own registry publishes.
#[test]
fn the_current_field_is_this_panes_own_product_as_the_registry_spells_it() {
    let h = handler_with(MrmsProduct::ReflectivityComposite, vec![45.0; 4]);
    let state = pane_state(MrmsProduct::PrecipRate);
    let pane = PaneRef {
        state: Some(&*state),
        ..PaneRef::bare(0)
    };
    let field = h.current_field(&pane).expect("a layer with products");
    assert_eq!(
        field,
        crate::mrms::fields::spec(MrmsProduct::PrecipRate)
            .id
            .clone(),
        "the answer must be THIS PANE's product, not the registry copy's — \
         the pane holds the rate and the handler's own default is the composite",
    );
    assert!(h.products().iter().any(|spec| spec.id == field));
}

#[test]
fn the_layer_declares_the_weight_and_the_cadence_it_was_registered_with() {
    let h = MrmsHandler::new();
    assert_eq!(h.id(), known::MRMS);
    assert_eq!(
        h.draw_order_weight(),
        15,
        "above the model, below outlooks — and since WB-10 this weight is \
         half the clock ruling, pinned with its rationale in squallar-egui's \
         radar_takes_the_clock_wherever_it_is_drawn",
    );
    assert_eq!(
        h.auto_poll_interval(),
        Some(120),
        "MRMS publishes every ~2 minutes",
    );
    assert_eq!(
        h.time_axis(),
        squallar_source::time::TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(120),
            extends_future: false,
        },
        "the ~2-minute mosaic cadence, stopping at the wall clock (WB-10)",
    );
    assert_eq!(
        h.min_loop_frames(),
        0,
        "NO floor, deliberately: at a 120 s step the Lookback slider's \
         default 3600 s already buys ~30 frames, so the slider governs and a \
         floor would widen every MRMS pane's rail for nothing. This is the \
         opposite call from GMGSI's 13, made for the opposite cadence.",
    );
    assert_eq!(
        h.min_loop_span_secs(),
        0,
        "no floor means the layer is untouched by loop_span_secs_for",
    );
    assert_eq!(
        h.frame_horizon(&PaneRef::across(&[])),
        chrono::Duration::zero(),
        "a mosaic exists for an instant that has happened and for no other",
    );
    assert_eq!(h.render_mode(), RenderMode::Texture);
    assert_eq!(h.job_codec().map(|row| row.label), Some("overlay/model"));
}

// ── Reopen is 1:1 ───────────────────────────────────────────────────────────

#[test]
fn every_pane_state_field_survives_a_save_and_a_reload() {
    let h = MrmsHandler::new();
    for enabled in [true, false] {
        for &product in MrmsProduct::all() {
            let saved = MrmsPaneState {
                enabled,
                selected_product: product,
            };
            let json = h.serialize_pane_state(&saved as &dyn std::any::Any);
            let restored = h
                .deserialize_pane_state(json, !enabled)
                .expect("this layer keeps pane state");
            let restored = restored
                .downcast_ref::<MrmsPaneState>()
                .expect("its own type back");
            assert_eq!(
                *restored, saved,
                "reopen is exactly 1:1, and the slot flag ({}) must not \
                 override what was saved",
                !enabled,
            );
        }
    }
}

/// A config written before this layer existed loads with the layer's own
/// defaults rather than refusing.
#[test]
fn an_empty_saved_state_falls_back_to_the_panes_slot_flag() {
    let h = MrmsHandler::new();
    let restored = h
        .deserialize_pane_state(serde_json::json!({}), true)
        .expect("pane state");
    let restored = restored.downcast_ref::<MrmsPaneState>().unwrap();
    assert!(
        restored.enabled,
        "the slot flag stands in for a missing key"
    );
    assert_eq!(
        restored.selected_product,
        MrmsProduct::ReflectivityComposite,
    );
}

// ── The sentinels, at the layer's own surface ───────────────────────────────

/// **What the NaN mapping actually buys at this layer**: a no-coverage point
/// answers no tooltip at all, rather than "CREF: -999.0 dBZ".
///
/// This is the consequence to check, and it is not the one the plan named. With
/// `gridded::color_for` — transparent below the first stop — an unmapped −999
/// would *not* paint, because the reflectivity bar starts at 5 dBZ. It would
/// show up here, in the reading a hover reports, and in `value_range`.
#[test]
fn a_no_coverage_point_reports_no_reading() {
    let h = handler_with(
        MrmsProduct::ReflectivityComposite,
        vec![f32::NAN, f32::NAN, 45.0, f32::NAN],
    );
    let coords = &h
        .cached_grids
        .get(MrmsProduct::ReflectivityComposite)
        .unwrap()
        .grid
        .coords;
    let (miss_lat, miss_lon) = coords.at(0).unwrap();
    assert_eq!(
        h.hover_value_at(miss_lat, miss_lon, &PaneRef::bare(0)),
        None,
        "a missing point must not read back as a value",
    );
    let (hit_lat, hit_lon) = coords.at(2).unwrap();
    assert_eq!(
        h.hover_value_at(hit_lat, hit_lon, &PaneRef::bare(0)),
        Some("CREF: 45.0 dBZ".to_string()),
        "non-triviality: a real reading DOES report, so the None above is \
         about the missing point and not about the hover path being dead",
    );
}

/// And the summary keeps them out of the range, so the blank notice cannot
/// quote a sentinel back at the user as a measurement.
#[test]
fn the_summary_excludes_missing_points_from_the_range() {
    let h = handler_with(
        MrmsProduct::ReflectivityComposite,
        vec![f32::NAN, 12.0, f32::NAN, 48.0],
    );
    let grid = h
        .cached_grids
        .get(MrmsProduct::ReflectivityComposite)
        .unwrap();
    assert_eq!(grid.value_range, Some((12.0, 48.0)));
    assert_eq!(grid.visible_points, 2);
    assert_eq!(grid.blank_notice(), None);
}

/// A mosaic that decoded fine and draws nothing says so — a quiet night and a
/// fetch that never happened look identical on screen otherwise.
#[test]
fn a_mosaic_with_nothing_above_the_first_band_explains_itself() {
    let h = handler_with(MrmsProduct::ReflectivityComposite, vec![1.0, 2.0, 3.0]);
    let notice = h
        .cached_grids
        .get(MrmsProduct::ReflectivityComposite)
        .unwrap()
        .blank_notice()
        .expect("a blank mosaic explains itself");
    assert!(notice.contains("lowest colour band"), "{notice}");
    assert!(
        h.controls(&PaneRef::bare(0))
            .iter()
            .any(|item| matches!(item, ControlItem::InfoText { text } if text == &notice)),
        "the notice must reach the panel, not only the log",
    );
}

// ── Controls ────────────────────────────────────────────────────────────────

/// Switching to a **resident** product needs no network; switching to one that
/// is not resident does.
#[test]
fn switching_products_fetches_only_what_is_not_already_in_hand() {
    let mut h = handler_with(MrmsProduct::ReflectivityComposite, vec![45.0; 4]);
    let before = h.state.data_generation;

    let effect = h.apply_control(
        &ControlUpdate {
            id: "product",
            value: ControlValue::String(MrmsProduct::PrecipRate.as_str().into()),
        },
        &mut PaneMut::bare(0),
    );
    assert!(
        matches!(effect, ControlEffect::Fetch),
        "the rate is not resident, so the pane must ask for it",
    );

    let effect = h.apply_control(
        &ControlUpdate {
            id: "product",
            value: ControlValue::String(MrmsProduct::ReflectivityComposite.as_str().into()),
        },
        &mut PaneMut::bare(0),
    );
    assert!(
        matches!(effect, ControlEffect::None),
        "the composite is still resident, so switching back must redraw \
         rather than refetch",
    );
    assert_ne!(
        h.state.data_generation, before,
        "the redraw needs a new content signature, or the pane keeps its old \
         texture",
    );
}

/// An option value the build does not register is ignored rather than parsed
/// into a panic — a newer build's product arriving through a saved config.
#[test]
fn an_unknown_product_id_is_ignored() {
    let mut h = handler_with(MrmsProduct::ReflectivityComposite, vec![45.0; 4]);
    let effect = h.apply_control(
        &ControlUpdate {
            id: "product",
            value: ControlValue::String("mrms_from_the_future".into()),
        },
        &mut PaneMut::bare(0),
    );
    assert!(matches!(effect, ControlEffect::None));
    assert_eq!(
        h.defaults.selected_product,
        MrmsProduct::ReflectivityComposite,
    );
}

/// The dropdown's option values are exactly the ids the registry publishes, so
/// a catalogue tile's `FieldId` can be sent straight through `apply_control`.
#[test]
fn the_product_dropdown_offers_the_registered_field_ids() {
    let h = handler_with(MrmsProduct::ReflectivityComposite, vec![45.0; 4]);
    let options: Vec<String> = h
        .controls(&PaneRef::bare(0))
        .into_iter()
        .find_map(|item| match item {
            ControlItem::Dropdown {
                id: "product",
                options,
                ..
            } => Some(options.into_iter().map(|(v, _)| v.to_string()).collect()),
            _ => None,
        })
        .expect("the product dropdown");
    let registered: Vec<String> = h
        .products()
        .iter()
        .map(|spec| spec.id.as_str().to_string())
        .collect();
    assert_eq!(options, registered);
    assert_eq!(h.field_control_id(), Some("product"));
}

/// The toggle names the mosaic's own **valid** time, not the fetch time: on a
/// two-minute cadence those are different facts and the first is on screen.
#[test]
fn the_toggle_label_carries_the_mosaics_valid_time() {
    let h = handler_with(MrmsProduct::ReflectivityComposite, vec![45.0; 4]);
    let label = h
        .controls(&PaneRef::bare(0))
        .into_iter()
        .find_map(|item| match item {
            ControlItem::Toggle { label, .. } => Some(label),
            _ => None,
        })
        .expect("a toggle");
    assert_eq!(label, "MRMS Mosaic (00:00z)");

    let bare = MrmsHandler::new();
    let label = bare
        .controls(&PaneRef::bare(0))
        .into_iter()
        .find_map(|item| match item {
            ControlItem::Toggle { label, .. } => Some(label),
            _ => None,
        })
        .expect("a toggle");
    assert_eq!(label, "MRMS Mosaic", "no data, no time to claim");
}

// ── Cross-pane ──────────────────────────────────────────────────────────────

/// Two panes on two products get two content signatures, or the render dispatch
/// groups them and draws one raster for both.
#[test]
fn two_panes_on_two_products_do_not_share_a_content_signature() {
    let h = handler_with(MrmsProduct::ReflectivityComposite, vec![45.0; 4]);
    let a = pane_state(MrmsProduct::ReflectivityComposite);
    let b = pane_state(MrmsProduct::PrecipRate);
    let pane_a = PaneRef {
        state: Some(&*a),
        ..PaneRef::bare(0)
    };
    let pane_b = PaneRef {
        state: Some(&*b),
        ..PaneRef::bare(1)
    };
    assert_ne!(h.content_signature(&pane_a), h.content_signature(&pane_b));
}

/// An arrival must not blank another pane: the pin is the **union** across
/// panes, taken from the panes rather than from the handler's own copy.
#[test]
fn an_arrival_pins_every_panes_product_not_just_its_own() {
    let h = handler_with(MrmsProduct::ReflectivityComposite, vec![45.0; 4]);
    let a = pane_state(MrmsProduct::ReflectivityComposite);
    let b = pane_state(MrmsProduct::PrecipRate);
    let states: Vec<&dyn std::any::Any> = vec![&*a, &*b];
    let pinned = h.pinned_products(&PaneRef::across(&states));
    assert!(pinned.contains(&MrmsProduct::ReflectivityComposite));
    assert!(pinned.contains(&MrmsProduct::PrecipRate));
    assert_eq!(pinned.len(), 2, "deduplicated, and both panes counted");
}

// -- The frame contract (WB-10) ---------------------------------------------

/// Stamp `k` of the fixture timeline: **not clock-aligned and not evenly
/// stepped**, exactly like the bucket's own (`000039`, `000242`, `000442`).
/// A 121 s stride from a :39 start means any code that rounds a stamp to the
/// 2-minute grid — the trait's `typical_step` is *typical*, not a promise —
/// files it under an instant nothing listed, and these tests go red.
fn t(k: i64) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 21)
        .unwrap()
        .and_hms_opt(0, 0, 39)
        .unwrap()
        + chrono::Duration::seconds(121 * k)
}

/// A granule for `t(k)` whose **values name the stamp**, so two frames' grids
/// can never be mistaken for one another however they are compared.
fn granule_at(product: MrmsProduct, k: i64, n: usize) -> MrmsGrid {
    let mut g = grid_of(product, vec![k as f32; n]);
    g.valid = t(k);
    g
}

/// The object key a listing would have found for `t(k)` — the real bucket
/// shape, via the same helper production keys go through, so
/// `fetch::key_valid_time` round-trips it.
fn object_key(product: MrmsProduct, k: i64) -> String {
    squallar_source::origins::DataSources::mrms_key(product.prefix_name(), &t(k))
}

/// Hand the handler the listing its own `create_frame_list_task` would have
/// produced for stamps `0..count`, on the one production door.
fn file_listing(h: &mut MrmsHandler, product: MrmsProduct, count: i64, complete: bool) {
    let range = (t(0), t(count - 1));
    let keys: Vec<(chrono::NaiveDateTime, String)> =
        (0..count).map(|k| (t(k), object_key(product, k))).collect();
    h.apply_frame_listing(
        FrameListing {
            range,
            frames: keys
                .iter()
                .map(|(valid, _)| FrameStamp {
                    valid: *valid,
                    run: None,
                })
                .collect(),
            complete,
        },
        Box::new(MrmsListing {
            product,
            range,
            keys,
            complete,
        }),
        &PaneRef::across(&[]),
    );
}

/// Deliver one frame's granule on the one production door.
fn file_frame(h: &mut MrmsHandler, product: MrmsProduct, k: i64, n: usize) {
    h.apply_frame(
        FrameStamp {
            valid: t(k),
            run: None,
        },
        Box::new(MrmsFrameFetch {
            product,
            valid: t(k),
            grid: Some(granule_at(product, k, n)),
        }),
        &PaneRef::across(&[]),
    );
}

fn frame_ctx(k: i64) -> RasterizeContext {
    RasterizeContext {
        frame: Some(FrameStamp {
            valid: t(k),
            run: None,
        }),
        ..rctx()
    }
}

/// The values the job describes, so a raster's identity can be read without
/// comparing 49 MB by hand.
fn job_values(job: &DescribedJob) -> Vec<f32> {
    let input = job
        .downcast_ref::<rasterize::GriddedInput>()
        .expect("the gridded carry");
    let rasterize::GriddedInput::Resident(grid) = input else {
        panic!("MRMS must describe a Resident carry, not {input:?}");
    };
    grid.values.to_f32()
}

/// A `FetchConfig` with nothing behind it: every test here stops before the
/// network. `tls::client` and not `reqwest::Client::new()`, which panics for
/// want of a crypto provider outside a whole-lib run.
fn fetch_config() -> FetchConfig {
    FetchConfig {
        client: squallar_source::tls::client(
            squallar_source::tls::USER_AGENT,
            std::time::Duration::from_secs(1),
        )
        .build()
        .expect("a client with a crypto provider installed"),
        zone_cache_dir: None,
        sources: squallar_source::origins::DataSources::default(),
        viewport: None,
        as_of: chrono::Utc::now().naive_utc(),
        depicted_span_secs: None,
        depicted_frames: Vec::new(),
    }
}

/// **The correctness pin of the whole item.** A named frame is rasterized from
/// **that frame's** granule; the live dispatch is untouched; and a frame whose
/// granule is not staged describes nothing rather than the pane's picture.
///
/// Values, not `Arc::ptr_eq`: the failure this guards is every frame receiving
/// the *same* picture, and a pointer test can be satisfied by the wrong shared
/// granule.
///
/// **Floor — `ignore_the_frame`:** delete the `ctx.frame` arm of `prepare_job`
/// so it always reads the live cache.
#[test]
fn a_named_frame_is_rasterized_from_that_frames_granule_and_not_the_panes() {
    let product = MrmsProduct::ReflectivityComposite;
    // The live picture is a granule NO frame shares, so a fallback to it is
    // visible rather than coincidentally right.
    let mut h = handler_with(product, vec![77.0; 8]);
    file_listing(&mut h, product, 13, true);
    // A staging area with room for both, so what is asserted is the LOOKUP and
    // not the eviction policy, which has its own test below.
    h.frame_grids = MrmsFrameCache::new(4 * 8 * 4, staging::global());
    file_frame(&mut h, product, 3, 8);
    file_frame(&mut h, product, 9, 8);

    let pane = PaneRef::across(&[]);
    let at = |k: i64| {
        job_values(
            &h.prepare_job(&frame_ctx(k), &pane)
                .expect("a staged frame describes a job"),
        )
    };
    assert_eq!(
        at(3),
        vec![3.0f32; 8],
        "the frame at t(3) was drawn from another stamp's granule"
    );
    assert_eq!(
        at(9),
        vec![9.0f32; 8],
        "the frame at t(9) was drawn from another stamp's granule"
    );
    assert_ne!(
        at(3),
        at(9),
        "two frames of one loop described the SAME picture; every frame of the \
         loop would be the same image and nothing else in the build detects it"
    );

    // The live dispatch is unmoved: `frame: None` still describes the pane's
    // own selection, which is what every non-looping pane does.
    assert_eq!(
        job_values(&h.prepare_job(&rctx(), &pane).expect("the live picture")),
        vec![77.0f32; 8],
    );

    // And a listed frame with no granule staged describes NOTHING, rather than
    // handing this stamp the live picture under another stamp's label.
    assert!(
        h.prepare_job(&frame_ctx(5), &pane).is_none(),
        "an unstaged frame fell back to the pane's picture: one instant's \
         mosaic presented, unlabelled, as another's"
    );
}

/// **Residency does not grow with the frame count.**
///
/// Thirty frames are listed — the Lookback slider's default hour at the
/// ~2-minute cadence — thirty granules are delivered one after another exactly
/// as the serialised fetch delivers them, and the layer never holds more than
/// the staging budget buys: one mosaic.
///
/// The byte arithmetic, with its denominator: **one mosaic is
/// `7000 * 3500 * 2` = 49,000,000 B (46.73 MiB)** at the 16-bit width MRMS
/// publishes, so thirty resident would be 1,470,000,000 B — 14.6x a 96 MiB wasm
/// pool. (It was `* 4` = 98,000,000 B and 29x while the store was `f32`; the
/// narrowing halved the figure and did not change the conclusion.) The loop's
/// own storage is thirty *textures*, which is a different budget in a different
/// crate.
///
/// **Floor — `stage_every_frame`:** delete the eviction loop in
/// `MrmsFrameCache::insert`.
#[test]
fn the_layer_stages_one_granule_however_many_frames_the_loop_holds() {
    let product = MrmsProduct::ReflectivityComposite;
    // Room for exactly one 8-value grid: the shipped ratio, at a size a test
    // can reach.
    let mut h = MrmsHandler::with_frame_budget(4 * 8);
    h.defaults.enabled = true;
    h.defaults.selected_product = product;
    file_listing(&mut h, product, 30, true);

    let pane = PaneRef::across(&[]);
    let ctx = fetch_config();
    assert_eq!(
        h.list_frames(&ctx, &pane, (t(0), t(29))).frames.len(),
        30,
        "premise: the listing must have named thirty frames, or there is no \
         frame count for residency to fail to track"
    );

    for k in 0..30 {
        file_frame(&mut h, product, k, 8);
        assert_eq!(
            h.frame_grids.len(),
            1,
            "after {} granules the layer holds {}. One mosaic is \
             7000 x 3500 x 2 = {} B, so {} resident is {} B against a 96 MiB \
             wasm pool. A loop holds textures, not grids.",
            k + 1,
            h.frame_grids.len(),
            crate::mrms::CONUS_GRID_BYTES,
            h.frame_grids.len(),
            h.frame_grids.len() * crate::mrms::CONUS_GRID_BYTES,
        );
        assert_eq!(
            h.frames_resident(&pane),
            vec![FrameStamp {
                valid: t(k),
                run: None
            }],
            "the one staged granule must be the one that just landed"
        );
    }

    // The shipped arithmetic the fixture stands in for.
    assert_eq!(
        crate::mrms::FRAME_STAGING_BYTES,
        crate::mrms::CONUS_GRID_BYTES,
        "one mosaic stages at a time on every arm"
    );
    assert_eq!(
        30 * crate::mrms::CONUS_GRID_BYTES,
        1_470_000_000,
        "thirty resident granules, spelled out"
    );
}

/// **One granule at a time is enough for the pipeline to advance**, which is
/// the claim the staging budget rests on: each arrival is described into a job
/// before the next lands, and the job keeps its own refcount, so the picture
/// survives the eviction the very next arrival causes.
///
/// **Floor — `describe_after_the_flood`:** move the `prepare_job` calls below
/// the loop that delivers all the granules.
#[test]
fn a_granule_evicted_after_its_job_is_described_still_paints_that_frame() {
    let product = MrmsProduct::ReflectivityComposite;
    let mut h = MrmsHandler::with_frame_budget(4 * 8);
    h.defaults.enabled = true;
    h.defaults.selected_product = product;
    file_listing(&mut h, product, 13, true);
    let pane = PaneRef::across(&[]);

    let mut described: Vec<Vec<f32>> = Vec::new();
    for k in 0..13 {
        file_frame(&mut h, product, k, 8);
        let job = h
            .prepare_job(&frame_ctx(k), &pane)
            .expect("the granule that just landed is the one staged");
        described.push(job_values(&job));
    }

    assert_eq!(
        described,
        (0..13).map(|k| vec![k as f32; 8]).collect::<Vec<_>>(),
        "thirteen frames must have described thirteen different pictures"
    );
    assert_eq!(
        h.frame_grids.len(),
        1,
        "and one granule was resident the whole way through"
    );
}

/// A fetch asks for one granule by the key its listing found: **1 GET, no
/// second LIST** — and declines a stamp nothing listed, or one already staged.
#[test]
fn a_frame_fetch_is_declined_where_there_is_no_key_and_where_the_granule_is_held() {
    let product = MrmsProduct::ReflectivityComposite;
    let mut h = MrmsHandler::with_frame_budget(4 * 8);
    h.defaults.enabled = true;
    h.defaults.selected_product = product;
    let pane = PaneRef::across(&[]);
    let ctx = fetch_config();
    let ask = |h: &MrmsHandler, k: i64| {
        h.fetch_frame(
            &ctx,
            &pane,
            &FrameStamp {
                valid: t(k),
                run: None,
            },
        )
        .is_some()
    };

    assert!(
        !ask(&h, 3),
        "nothing has been listed, so there is no key to GET — and a stamp \
         cannot be rounded into one, because the bucket's timestamps are not \
         clock-aligned"
    );
    file_listing(&mut h, product, 13, true);
    assert!(ask(&h, 3), "a listed stamp is one GET away");
    assert!(
        !ask(&h, 99),
        "a stamp outside every listing is declined rather than guessed at"
    );
    file_frame(&mut h, product, 3, 8);
    assert!(!ask(&h, 3), "a staged granule is not fetched twice");
}

/// `list_frames` says `complete` only where a listing that really covered the
/// window landed. A sampled or failed one leaves the answer readable as "at
/// least these", which is what it is.
#[test]
fn an_incomplete_listing_never_settles_the_window() {
    let product = MrmsProduct::ReflectivityComposite;
    let mut h = MrmsHandler::new();
    h.defaults.enabled = true;
    h.defaults.selected_product = product;
    let pane = PaneRef::across(&[]);
    let ctx = fetch_config();

    file_listing(&mut h, product, 13, false);
    let listed = h.list_frames(&ctx, &pane, (t(0), t(12)));
    assert_eq!(listed.frames.len(), 13, "the keys are filed either way");
    assert!(
        !listed.complete,
        "an incomplete listing must not claim the window is settled"
    );

    file_listing(&mut h, product, 13, true);
    assert!(h.list_frames(&ctx, &pane, (t(0), t(12))).complete);
    // A window wider than anything covered is still open.
    assert!(!h.list_frames(&ctx, &pane, (t(0), t(40))).complete);
}

/// Frames are scoped to the pane's own product: another product's granule is
/// not a frame this pane can draw, and offering it would paint the wrong
/// field.
#[test]
fn one_products_frames_are_not_offered_to_a_pane_on_another() {
    let mut h = MrmsHandler::with_frame_budget(4 * 8 * 4);
    h.defaults.enabled = true;
    h.defaults.selected_product = MrmsProduct::ReflectivityComposite;
    file_listing(&mut h, MrmsProduct::ReflectivityComposite, 13, true);
    file_listing(&mut h, MrmsProduct::PrecipRate, 13, true);
    file_frame(&mut h, MrmsProduct::ReflectivityComposite, 3, 8);
    file_frame(&mut h, MrmsProduct::PrecipRate, 4, 8);

    let cref = pane_state(MrmsProduct::ReflectivityComposite);
    let rate = pane_state(MrmsProduct::PrecipRate);
    let pane_cref = PaneRef {
        state: Some(&*cref),
        ..PaneRef::bare(0)
    };
    let pane_rate = PaneRef {
        state: Some(&*rate),
        ..PaneRef::bare(1)
    };
    assert_eq!(
        h.frames_resident(&pane_cref),
        vec![FrameStamp {
            valid: t(3),
            run: None
        }],
    );
    assert_eq!(
        h.frames_resident(&pane_rate),
        vec![FrameStamp {
            valid: t(4),
            run: None
        }],
    );
    assert_eq!(
        h.frame_grids.len(),
        2,
        "both are staged; what differs is which pane is offered which"
    );
}

/// `retain_frames` is the eviction door, and it drops this pane's product
/// alone. **Nothing above calls it yet**, which is why it is driven here.
#[test]
fn retain_frames_drops_this_products_unkept_granules_and_no_others() {
    let mut h = MrmsHandler::with_frame_budget(4 * 8 * 8);
    h.defaults.enabled = true;
    h.defaults.selected_product = MrmsProduct::ReflectivityComposite;
    file_listing(&mut h, MrmsProduct::ReflectivityComposite, 13, true);
    for k in 0..3 {
        file_frame(&mut h, MrmsProduct::ReflectivityComposite, k, 8);
    }
    file_frame(&mut h, MrmsProduct::PrecipRate, 7, 8);
    assert_eq!(h.frame_grids.len(), 4, "premise: four granules are staged");

    h.retain_frames(
        &PaneRef::across(&[]),
        &[FrameStamp {
            valid: t(1),
            run: None,
        }],
    );
    assert_eq!(
        h.frames_resident(&PaneRef::across(&[])),
        vec![FrameStamp {
            valid: t(1),
            run: None
        }],
    );
    assert_eq!(
        h.frame_grids.len(),
        2,
        "the other product's granule belongs to another pane and is untouched"
    );
}

/// **The gate serialises, and it releases.** `MrmsHandler::fetch_frame`'s doc
/// claims the whole render set may be dispatched at once while only one
/// granule is ever in flight; this is the floor under that claim. The stakes
/// are higher than GMGSI's: the staging slot holds one 49 MB values vector and
/// every decode that misses it allocates its own, so thirty concurrent fetches
/// — one slider-default hour — would be ~1.5 GB in flight before any cache saw
/// a byte.
///
/// Two fetch tasks are driven by hand inside one thread — `futures::poll!`
/// with `yield_now` turns between, so "the second gets N chances" is a poll
/// count and never a wall clock — against a loopback server that records
/// every request line and **holds t(0)'s response open** until told:
///
/// 1. A is polled until its GET is on the wire, then parked mid-response;
/// 2. B is polled 50,000 times and must not issue its GET — the gate holds it;
/// 3. the server releases, A completes, and B **does** proceed — so a gate
///    that never releases (a guard held across an `.await` that never
///    resolves) fails here, not just the mutation that removes it.
///
/// The bodies are not granules; both fetches complete as `grid: None`, which
/// exercises exactly what this test is about — the wire, not the decode.
///
/// **Floor — `no_gate`:** `let _one_at_a_time = gate.lock().await;` ->
/// `let _one_at_a_time = ();`.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn two_frame_fetches_share_one_gate_and_the_second_waits_for_the_first() {
    use std::sync::Mutex;

    // Bound here; **served on the test's own runtime**, below — see the
    // comment there. It is a measured flake, not a style preference.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    let seen: Arc<Mutex<Vec<String>>> = Default::default();
    // t(0)'s key ends in its own second-precision stamp, which is what
    // appears on the request line.
    let held_stamp = t(0).format("%Y%m%d-%H%M%S").to_string();

    let product = MrmsProduct::ReflectivityComposite;
    let mut h = MrmsHandler::new();
    h.defaults.enabled = true;
    h.defaults.selected_product = product;
    file_listing(&mut h, product, 2, true);

    // `tls::client` sets `https_only`, which a loopback URL cannot satisfy;
    // `tls::init` is still required because reqwest is pinned to
    // `rustls-no-provider`. Timeout wide enough that a loaded machine cannot
    // time the held response out mid-test.
    squallar_source::tls::init();
    let cfg = FetchConfig {
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("a cleartext loopback client"),
        zone_cache_dir: None,
        sources: squallar_source::origins::DataSources {
            mrms_bucket: "mrms".into(),
            s3_base: format!("http://127.0.0.1:{port}/{{bucket}}").into(),
            ..squallar_source::origins::DataSources::production()
        },
        viewport: None,
        as_of: chrono::Utc::now().naive_utc(),
        depicted_span_secs: None,
        depicted_frames: Vec::new(),
    };
    let pane = PaneRef::across(&[]);
    let mut a = h
        .fetch_frame(
            &cfg,
            &pane,
            &FrameStamp {
                valid: t(0),
                run: None,
            },
        )
        .expect("t(0) is listed")
        .future;
    let mut b = h
        .fetch_frame(
            &cfg,
            &pane,
            &FrameStamp {
                valid: t(1),
                run: None,
            },
        )
        .expect("t(1) is listed")
        .future;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    rt.block_on(async move {
        // -- The bucket, served ON THIS RUNTIME AND THIS THREAD -------------
        //
        // **It used to be two OS threads, and that is what made this test
        // flaky.** Every loop below bounds itself by a POLL COUNT and never a
        // clock, which is right — but the thing each loop is waiting for was
        // an OS thread being scheduled (accept, then read, then push the
        // request line), while `tokio::task::yield_now()` on a
        // `new_current_thread` runtime yields to the RUNTIME and never
        // releases the OS thread. So the loop spun 200,000 times at 100% of
        // one core, on a box where every core was already taken, and reached
        // its cap before the server threads got a slice. It then failed
        // saying "the first fetch never issued its GET" — which was not what
        // happened: the GET was on the wire and had not been RECORDED yet.
        //
        // On this runtime the count means what it says. The server is two
        // tasks on the same thread as the fetches, so each `yield_now` turn
        // is a real, guaranteed chance for accept and read to run. No sleep,
        // no clock, and no widened tolerance. Same change, same reason, as
        // the GMGSI twin of this test.
        listener
            .set_nonblocking(true)
            .expect("a freshly bound listener can be made non-blocking");
        let listener = tokio::net::TcpListener::from_std(listener)
            .expect("this runtime adopts the bound listener");
        // A permit, not an edge: `notify_one` before the connection task
        // reaches `notified()` stores the permit rather than losing it.
        let release = Arc::new(tokio::sync::Notify::new());
        let recorder = Arc::clone(&seen);
        let held = Arc::clone(&release);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let recorder = Arc::clone(&recorder);
                let held = Arc::clone(&held);
                let held_stamp = held_stamp.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut scratch = [0u8; 4096];
                    let read = stream.read(&mut scratch).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&scratch[..read]).to_string();
                    let line = request.lines().next().unwrap_or("").to_string();
                    let hold = line.contains(&held_stamp);
                    recorder
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(line);
                    if hold {
                        held.notified().await;
                    }
                    let body = "not a granule; the decode failing is fine here";
                    let _ = stream
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\
                                 Connection: close\r\n\r\n{body}",
                                body.len(),
                            )
                            .as_bytes(),
                        )
                        .await;
                });
            }
        });

        let gets = |seen: &Arc<Mutex<Vec<String>>>| -> usize {
            seen.lock().unwrap_or_else(|e| e.into_inner()).len()
        };

        // 1. A acquires the gate and puts its GET on the wire, where the
        // server holds it. The cap is a poll count, not a clock.
        let mut turns = 0usize;
        while gets(&seen) == 0 {
            assert!(
                turns < 200_000,
                "the loopback server recorded no request line in {turns} \
                 driver turns, each of which was also a poll of the accept \
                 and read tasks on this same thread. The first fetch never \
                 put its GET on the wire",
            );
            assert!(
                futures::poll!(a.as_mut()).is_pending(),
                "t(0) completed while the server was holding its response",
            );
            tokio::task::yield_now().await;
            turns += 1;
        }

        // 2. Fifty thousand chances for B while A is mid-flight. On the
        // gated build it parks on the lock and its GET never goes out; on
        // the ungated one it connects within a few driver turns and
        // completes, which the count below turns red.
        for _ in 0..50_000 {
            if futures::poll!(b.as_mut()).is_ready() {
                break;
            }
            tokio::task::yield_now().await;
        }
        let in_flight = gets(&seen); // a local, never asserted under the lock
        assert_eq!(
            in_flight, 1,
            "the second frame's GET was issued while the first granule was \
             still in flight ({in_flight} request lines recorded). \
             Unserialised, a thirty-frame render set holds a 49 MB values \
             vector EACH — the staging slot can serve exactly one — so ~1.5 GB \
             in flight before any cache can evict anything.",
        );

        // 3. Release the held response; A completes and drops the guard.
        release.notify_one();
        let mut turns = 0usize;
        loop {
            if futures::poll!(a.as_mut()).is_ready() {
                break;
            }
            assert!(
                turns < 200_000,
                "the held fetch never completed after release"
            );
            tokio::task::yield_now().await;
            turns += 1;
        }

        // 4. Non-triviality: B now proceeds. A gate that serialises by never
        // releasing — a guard held across an await that never resolves —
        // starves every later frame and fails here.
        let mut turns = 0usize;
        loop {
            if futures::poll!(b.as_mut()).is_ready() {
                break;
            }
            assert!(
                turns < 200_000,
                "the gate never released: the second frame's fetch is starved \
                 after the first completed",
            );
            tokio::task::yield_now().await;
            turns += 1;
        }

        let lines = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(lines.len(), 2, "one GET per frame, no relisting: {lines:?}");
        assert!(
            lines[0].contains(&t(0).format("%Y%m%d-%H%M%S").to_string())
                && lines[1].contains(&t(1).format("%Y%m%d-%H%M%S").to_string()),
            "the two GETs must be the two listed keys in dispatch order: {lines:?}",
        );
    });
}

/// **`latest_at` answers `FrameSeries`'s rule here too**, over a known listing
/// of thirty ~2-minute mosaics.
///
/// Same three edges as the satellite layer's pin of the same name, because it
/// is the same rule and there is one implementation of it.
///
/// **Floor** — the `t`-exactly-on-a-stamp case is asserted for every one of
/// the thirty stamps, and asserted to be that stamp rather than its
/// predecessor.
#[test]
fn latest_at_answers_the_frame_series_rule() {
    let product = MrmsProduct::ReflectivityComposite;
    let mut h = MrmsHandler::new();
    h.defaults.enabled = true;
    h.defaults.selected_product = product;
    let pane = PaneRef::across(&[]);
    file_listing(&mut h, product, 30, true);

    assert_eq!(
        h.latest_at(&pane, t(0) - chrono::Duration::seconds(1)),
        None,
        "one second before the oldest mosaic there is nothing to draw, and \
         the oldest mosaic is not it",
    );
    assert_eq!(
        h.latest_at(&pane, t(29) + chrono::Duration::hours(1)),
        Some(FrameStamp {
            valid: t(29),
            run: None,
        }),
        "an hour past the newest mosaic still draws the newest: nothing later \
         depicts anything",
    );
    for k in 0..30 {
        assert_eq!(
            h.latest_at(&pane, t(k)),
            Some(FrameStamp {
                valid: t(k),
                run: None,
            }),
            "the clock standing exactly on mosaic {k} draws mosaic {k}, never \
             mosaic {}",
            k - 1,
        );
    }
    assert_eq!(
        h.latest_at(&pane, t(7) + chrono::Duration::seconds(1)),
        Some(FrameStamp {
            valid: t(7),
            run: None,
        }),
        "one second after a mosaic's stamp is still drawn from that mosaic",
    );
}

/// **The satellite layer's blank leading frame, in the mosaic layer's own
/// window** — the identical clip, found by the contract rather than by a user.
///
/// `list_frames` clipped at `range.0`, so a window opened between two mosaics
/// came back without the one every stop in front of its first listed stamp is
/// drawn from. It never produced a report because this layer's cadence is
/// ~2 minutes rather than an hour, so the blank leading step was gone before
/// anyone could see it — a smaller instance of the same defect, not a
/// different one.
///
/// **Floors.** (a) A window opened exactly on a stamp reaches nothing extra —
/// it is the newest earlier mosaic, not a blanket step of slack. (b) Only one
/// earlier mosaic comes in, never the tail behind it. (c) The trailing edge is
/// untouched.
#[test]
fn a_listing_carries_the_mosaic_the_window_opened_after() {
    let product = MrmsProduct::ReflectivityComposite;
    let mut h = MrmsHandler::new();
    h.defaults.enabled = true;
    h.defaults.selected_product = product;
    let pane = PaneRef::across(&[]);
    let ctx = fetch_config();
    file_listing(&mut h, product, 30, true);

    let valid = |range| {
        h.list_frames(&ctx, &pane, range)
            .frames
            .into_iter()
            .map(|f| f.valid)
            .collect::<Vec<_>>()
    };

    // The stamps are 121 s apart, so a window opened one second after mosaic
    // 3 has 120 s of clock in front of mosaic 4.
    let one_second = chrono::Duration::seconds(1);
    assert_eq!(
        valid((t(3) + one_second, t(5))),
        vec![t(3), t(4), t(5)],
        "a window opened a second after mosaic 3 is drawn from mosaic 3 until \
         mosaic 4 lands, so mosaic 3 is the oldest frame it can be answered \
         with",
    );

    // Floor (a): opened exactly on a stamp, nothing extra.
    assert_eq!(
        valid((t(3), t(5))),
        vec![t(3), t(4), t(5)],
        "a window opened exactly on a mosaic already holds its own oldest \
         frame and must not reach behind it",
    );

    // Floor (b): one mosaic earlier, not the tail.
    assert_eq!(
        valid((t(28) + one_second, t(29))),
        vec![t(28), t(29)],
        "the newest earlier mosaic comes in, not every mosaic before the \
         window",
    );

    // Floor (c): the trailing edge is unchanged.
    assert_eq!(
        valid((t(3), t(5) + one_second)),
        vec![t(3), t(4), t(5)],
        "mosaic 6 depicts an instant later than anything the window reaches, \
         so it is not one of its frames",
    );
}

// ── The retained staging buffer ─────────────────────────────────────────────

/// A mosaic-shaped grid: `resident_bytes()` is a real 49 MB and its buffer is
/// exactly what [`staging::StagingPool`] retains.
///
/// The other grids in this file are one row wide, because the budgets they
/// exercise have to be overflowable. This one cannot be: what it is here to
/// exercise **is** the mosaic capacity rule, and a 400-byte grid would be
/// declined by the pool for exactly the reason the pool declines one in
/// production.
///
/// **And it is built on the narrow arm, which is load-bearing.** The pool's
/// slot is a `Vec<u16>`, so [`staging::StagingPool::recycle`] declines a
/// [`GridValues::F32`](crate::render::gridded::GridValues::F32) grid by design.
/// A wide fixture here would still compile and still run — it would simply
/// measure the *decline* path under three test names that say "reached the
/// pool", which is the shape a green vacuous gate takes.
fn mosaic_grid(product: MrmsProduct, valid: chrono::NaiveDateTime) -> MrmsGrid {
    let mut codes: Vec<u16> = Vec::new();
    codes
        .try_reserve_exact(staging::STAGING_POINTS)
        .expect("a mosaic buffer fits on a test host");
    codes.resize(staging::STAGING_POINTS, 0);
    // An affine that reads every code back as 45.0 dBZ — drawable, and no code
    // of it is a sentinel. The fixture's job is to be mosaic-sized, painted and
    // recyclable, not to be a second decoder.
    let scaled = crate::render::gridded::ScaledU16::new(codes, 45.0, 0.0, 1.0, |v| !v.is_finite())
        .expect("a packing that reserves no code takes the narrow arm");
    let mut grid = grid_of_values(product, crate::render::gridded::GridValues::Scaled(scaled));
    grid.valid = valid;
    grid
}

/// **The eviction that feeds the pool.** `MrmsFrameCache::insert` is the hot
/// path of a playing loop — one eviction per arriving granule — and it is where
/// the retained buffer comes from. Before this, the victim was dropped and the
/// next granule allocated a fresh mosaic block; that churn is what fragmented
/// the browser's 1 GiB heap until a mosaic request failed with 192 MB free.
///
/// Driven against a pool this test owns rather than the process-wide slot, for
/// the reason [`MrmsFrameCache::staging`] records: this binary also decodes the
/// `decode` fixtures through the global pool, and a filtered run here is not
/// self-contained.
///
/// **Floor — `drop_the_victim`:** restore a bare `self.entries.remove(&victim);`
/// with no `recycle`; the pool then reports nothing recycled and the take below
/// allocates.
#[test]
fn an_evicted_frame_granule_is_offered_to_the_staging_pool() {
    static POOL: staging::StagingPool = staging::StagingPool::new();
    // One mosaic of budget, which is the shipped `FRAME_STAGING_BYTES` — this
    // is the one frame-cache test that can afford the real figure.
    let mut h = MrmsHandler::with_frame_budget_and_staging(FRAME_STAGING_BYTES, &POOL);
    let product = MrmsProduct::ReflectivityComposite;
    let stamp = |m: u32| {
        chrono::NaiveDate::from_ymd_opt(2026, 8, 31)
            .expect("a real date")
            .and_hms_opt(0, m, 0)
            .expect("a real time")
    };

    h.frame_grids.insert(
        FrameKey {
            product,
            valid: stamp(0),
        },
        mosaic_grid(product, stamp(0)),
    );
    assert_eq!(
        POOL.totals(),
        staging::StagingTotals {
            allocated: 0,
            reused: 0,
            declined: 0
        },
        "premise: one granule fits the budget, so nothing has been evicted yet",
    );

    // The second arrival puts the store over one mosaic and evicts the first.
    h.frame_grids.insert(
        FrameKey {
            product,
            valid: stamp(2),
        },
        mosaic_grid(product, stamp(2)),
    );
    assert_eq!(h.frame_grids.len(), 1, "premise: the budget evicted one");

    let staged = POOL
        .take(staging::STAGING_POINTS)
        .expect("the slot is full");
    assert_eq!(
        POOL.totals(),
        staging::StagingTotals {
            allocated: 0,
            reused: 1,
            declined: 0
        },
        "the evicted granule's buffer must have reached the pool, so the next \
         mosaic decode is handed it instead of taking a fresh 49 MB block off \
         an allocator that can only grow",
    );
    assert_eq!(staged.capacity(), staging::STAGING_POINTS);
    assert!(staged.is_empty(), "and it arrives with nothing in it");
}

/// And the other door out of the store — `retain_frames`, the [`FrameSource`]
/// eviction authority — feeds the same pool.
#[test]
fn a_retained_frame_set_offers_the_dropped_granules_to_the_staging_pool() {
    static POOL: staging::StagingPool = staging::StagingPool::new();
    let mut h = MrmsHandler::with_frame_budget_and_staging(FRAME_STAGING_BYTES, &POOL);
    let product = MrmsProduct::ReflectivityComposite;
    let valid = chrono::NaiveDate::from_ymd_opt(2026, 8, 31)
        .expect("a real date")
        .and_hms_opt(0, 0, 0)
        .expect("a real time");
    h.frame_grids
        .insert(FrameKey { product, valid }, mosaic_grid(product, valid));

    h.retain_frames(&PaneRef::across(&[]), &[]);
    assert_eq!(h.frame_grids.len(), 0, "premise: the granule was dropped");
    assert_eq!(
        POOL.totals(),
        staging::StagingTotals {
            allocated: 0,
            reused: 0,
            declined: 0
        },
        "and its buffer went to the pool rather than back to the allocator",
    );
    assert_eq!(
        POOL.take(staging::STAGING_POINTS)
            .expect("the slot is full")
            .capacity(),
        staging::STAGING_POINTS,
    );
    assert_eq!(POOL.totals().reused, 1);
}

/// **The live path recycles too, and it only does because of the order.**
///
/// A live mosaic is held twice — by `cached_grids` and by `state.data` — so
/// the two-minute poll's replacement has a second owner at the moment the
/// cache hands it back, and `Arc::into_inner` answers `None`. Letting `state`
/// go first leaves the cache the sole owner. One granule every 120 s is not
/// what killed the page, but it is 49 MB back to an allocator that can only
/// grow, on the layer whose block size is the whole problem.
///
/// **Floor — `cache_first`:** restore
/// `cached_grids.insert(...); state.set_data(Some(arc));`; the pool then
/// reports the replacement declined and the take below allocates.
#[test]
fn the_live_mosaic_a_poll_replaces_is_offered_to_the_staging_pool() {
    static POOL: staging::StagingPool = staging::StagingPool::new();
    let mut h = MrmsHandler::with_staging(&POOL);
    let product = MrmsProduct::ReflectivityComposite;
    h.defaults.enabled = true;
    h.defaults.selected_product = product;

    let valid = chrono::NaiveDate::from_ymd_opt(2026, 8, 31)
        .expect("a real date")
        .and_hms_opt(0, 0, 0)
        .expect("a real time");
    h.apply_fetch_result(
        Box::new(MrmsFetchResult(Ok(mosaic_grid(product, valid)))),
        &PaneRef::across(&[]),
    );
    assert_eq!(
        POOL.totals(),
        staging::StagingTotals {
            allocated: 0,
            reused: 0,
            declined: 0
        },
        "premise: the first mosaic replaces nothing",
    );

    // The next poll of the same product, two minutes later.
    h.apply_fetch_result(
        Box::new(MrmsFetchResult(Ok(mosaic_grid(
            product,
            valid + chrono::Duration::seconds(120),
        )))),
        &PaneRef::across(&[]),
    );
    assert_eq!(
        POOL.totals(),
        staging::StagingTotals {
            allocated: 0,
            reused: 0,
            declined: 0
        },
        "the replaced mosaic's buffer must have reached the pool rather than \
         the allocator",
    );
    assert_eq!(
        POOL.take(staging::STAGING_POINTS)
            .expect("the slot is full")
            .capacity(),
        staging::STAGING_POINTS,
    );
    assert_eq!(POOL.totals().reused, 1);
}

/// **What the heap census reads**: the layer's own decoded bytes reach
/// [`OverlayRegistry::resident_source_bytes`], and the pane's carry of the same
/// mosaic is not added to the cache entry it *is*.
///
/// Against a pool of this test's own, not [`staging::global`]: the shipped slot
/// is process-wide, so a mosaic another test in this binary left in it would
/// add 49 MB to the figure asserted here and the failure would name the wrong
/// module.
#[test]
fn the_registry_sums_what_each_handler_is_holding() {
    static POOL: staging::StagingPool = staging::StagingPool::new();
    let product = MrmsProduct::ReflectivityComposite;
    let mut handler = MrmsHandler::with_staging(&POOL);
    handler.defaults.enabled = true;
    handler.defaults.selected_product = product;

    assert_eq!(
        handler.resident_source_bytes(),
        0,
        "a handler that has decoded nothing is holding nothing",
    );

    handler.apply_fetch_result(
        Box::new(MrmsFetchResult(Ok(sized(product, 100)))),
        &PaneRef::across(&[]),
    );
    assert_eq!(
        handler.resident_source_bytes(),
        400,
        "one 100-value mosaic, once: the live cache's entry and the pane's \
         carry are the same allocation, and adding both would price this \
         layer at twice the block it holds",
    );

    use crate::render::overlay_state::OverlayRegistry;
    let registry = OverlayRegistry::with_handlers(vec![Box::new(handler)]);
    assert_eq!(
        registry.resident_source_bytes(),
        400,
        "and the registry's sum is the handlers' figures, not a re-derivation",
    );
    assert_eq!(
        OverlayRegistry::default().resident_source_bytes(),
        0,
        "every other registered layer takes the trait's zero, so a build that \
         has fetched nothing prices its grids at nothing",
    );
}
