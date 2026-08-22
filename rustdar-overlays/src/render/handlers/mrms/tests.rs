use super::*;
use crate::render::gridded::ResidentGrid;
use rustdar_geo::GeoBounds;

const BOUNDS: GeoBounds = GeoBounds {
    min_lat: 34.0,
    max_lat: 36.0,
    min_lon: -99.0,
    max_lon: -97.0,
};

/// A small mosaic over [`BOUNDS`], `points` values wide by one row, so a test
/// can size a grid against a budget it can overflow.
fn grid_of(product: MrmsProduct, values: Vec<f32>) -> MrmsGrid {
    let ni = values.len().max(2);
    let spec = crate::mrms::fields::spec(product);
    let paint = crate::render::gridded::field_paint(&spec.id).expect("registered");
    let (visible_points, value_range) = crate::hrrr::summarize_values(&values, |v| paint.paints(v));
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

/// A mosaic of exactly `n` values, all drawable, so `resident_bytes()` is `4n`.
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

/// **The instrument that makes every eviction test below non-vacuous.** A cache
/// whose budget it cannot overflow proves nothing about eviction.
#[test]
fn the_budget_is_spent_in_bytes_and_a_grid_that_fits_is_kept() {
    // Room for exactly one 100-value grid (400 bytes).
    let mut cache = MrmsGridCache::new(400);
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
    let mut cache = MrmsGridCache::new(400);
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
    let mut cache = MrmsGridCache::new(800);
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
    let mut cache = MrmsGridCache::new(400);
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
    let mut cache = MrmsGridCache::new(400);
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

// ── The raster carry ────────────────────────────────────────────────────────

/// `prepare_job` hands over an **`Arc` clone**, not a copy: a 98 MB memcpy per
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
         so 98 MB was copied to describe it",
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
    assert_eq!(h.draw_order_weight(), 15, "above the model, below outlooks");
    assert_eq!(
        h.auto_poll_interval(),
        Some(120),
        "MRMS publishes every ~2 minutes",
    );
    assert!(
        matches!(h.time_axis(), rustdar_source::time::TimeAxis::Live),
        "a third FrameSeries layer needs a ruling; this one is Live",
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
