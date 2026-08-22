//! The GMGSI layer at its registration surface.
//!
//! The decode, the axes and the ramps are held in `crate::gmgsi`'s own suites
//! against the committed granule. What is left to this file is what only the
//! *layer* can be wrong about: the weight and clock it registers with, the byte
//! budget its cache spends, that describing a job costs a refcount rather than
//! 60 MB, and that reopen is 1:1.

use super::*;
use crate::gmgsi::decode::GmgsiGrid;
use crate::hrrr::GridCoords;

/// A small separable mosaic, `n` columns by one row, so a test can size a grid
/// against a budget it can actually overflow.
///
/// Separable and not regular: this layer's whole geometry is the arm GMGSI
/// forced, and a fixture on a different arm would not exercise it.
fn grid_of(channel: GmgsiChannel, values: Vec<f32>) -> GmgsiGrid {
    let ni = values.len().max(2);
    let spec = crate::gmgsi::fields::spec(channel);
    let lon_axis: Vec<f64> = (0..ni).map(|i| -99.0 + i as f64 * 0.072).collect();
    let lat_axis = vec![35.0f64];
    let bounds = GeoBounds {
        min_lat: 35.0,
        max_lat: 35.0,
        min_lon: *lon_axis.first().unwrap(),
        max_lon: *lon_axis.last().unwrap(),
    };
    GmgsiGrid {
        channel,
        grid: ResidentGrid {
            field: spec.id.clone(),
            ni,
            nj: 1,
            coords: GridCoords::Separable { lat_axis, lon_axis },
            values,
        },
        bounds,
        valid_time: chrono::NaiveDate::from_ymd_opt(2025, 6, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
    }
}

/// A mosaic of exactly `n` values, so `resident_bytes()` is `4n`.
fn sized(channel: GmgsiChannel, n: usize) -> GmgsiGrid {
    grid_of(channel, vec![82.0; n])
}

fn handler_with(channel: GmgsiChannel, values: Vec<f32>) -> GmgsiHandler {
    let mut h = GmgsiHandler::new();
    h.defaults.enabled = true;
    h.defaults.selected_channel = channel;
    h.apply_fetch_result(
        Box::new(GmgsiFetchResult(Ok(grid_of(channel, values)))),
        &PaneRef::across(&[]),
    );
    h
}

fn granule_of(channel: GmgsiChannel, n: usize) -> GmgsiGranule {
    let g = sized(channel, n);
    GmgsiGranule {
        grid: Arc::new(g.grid),
        bounds: g.bounds,
        valid_time: g.valid_time,
    }
}

fn pane_state(channel: GmgsiChannel) -> Box<GmgsiPaneState> {
    Box::new(GmgsiPaneState {
        enabled: true,
        selected_channel: channel,
    })
}

// -- The byte budget --------------------------------------------------------

/// **The instrument that makes every eviction test below non-vacuous.** A cache
/// whose budget it cannot overflow proves nothing about eviction, and the
/// shipped budget is 60 MB, which a test cannot fill.
#[test]
fn the_budget_is_spent_in_bytes_and_a_grid_that_fits_is_kept() {
    let mut cache = GmgsiGridCache::new(4 * 100);
    cache.insert(
        GmgsiChannel::LongwaveIr,
        granule_of(GmgsiChannel::LongwaveIr, 100),
        &[],
    );
    assert!(cache.get(GmgsiChannel::LongwaveIr).is_some());
    assert_eq!(cache.resident_bytes(), 400);
}

#[test]
fn a_grid_that_does_not_fit_evicts_the_least_recently_used_one() {
    // Room for two 100-value grids and no more.
    let mut cache = GmgsiGridCache::new(4 * 200);
    cache.insert(
        GmgsiChannel::LongwaveIr,
        granule_of(GmgsiChannel::LongwaveIr, 100),
        &[],
    );
    cache.insert(
        GmgsiChannel::ShortwaveIr,
        granule_of(GmgsiChannel::ShortwaveIr, 100),
        &[],
    );
    // Use longwave, making shortwave the least recently used.
    assert!(cache.get(GmgsiChannel::LongwaveIr).is_some());
    cache.insert(
        GmgsiChannel::Visible,
        granule_of(GmgsiChannel::Visible, 100),
        &[],
    );

    assert!(
        cache.get(GmgsiChannel::ShortwaveIr).is_none(),
        "the least recently used channel should have gone"
    );
    assert!(cache.get(GmgsiChannel::LongwaveIr).is_some());
    assert!(cache.get(GmgsiChannel::Visible).is_some());
}

/// All three fit when the budget widens — so the eviction above was the
/// *budget* and not a hidden entry cap.
#[test]
fn a_wider_budget_holds_every_grid() {
    let mut cache = GmgsiGridCache::new(4 * 300);
    for &c in &[
        GmgsiChannel::LongwaveIr,
        GmgsiChannel::ShortwaveIr,
        GmgsiChannel::Visible,
    ] {
        cache.insert(c, granule_of(c, 100), &[]);
    }
    assert_eq!(cache.len(), 3);
    assert_eq!(cache.resident_bytes(), 1200);
}

/// A pane's own channel must survive an arrival for a different one, or the
/// pane stops drawing and nothing re-asks.
#[test]
fn the_cache_never_evicts_a_pinned_channel() {
    let mut cache = GmgsiGridCache::new(4 * 200);
    cache.insert(
        GmgsiChannel::LongwaveIr,
        granule_of(GmgsiChannel::LongwaveIr, 100),
        &[],
    );
    cache.insert(
        GmgsiChannel::ShortwaveIr,
        granule_of(GmgsiChannel::ShortwaveIr, 100),
        &[],
    );
    // Longwave is pinned even though it is now the oldest use.
    assert!(cache.get(GmgsiChannel::ShortwaveIr).is_some());
    cache.insert(
        GmgsiChannel::Visible,
        granule_of(GmgsiChannel::Visible, 100),
        &[GmgsiChannel::LongwaveIr],
    );
    assert!(
        cache.get(GmgsiChannel::LongwaveIr).is_some(),
        "a pinned channel must not be evicted"
    );
    assert!(
        cache.get(GmgsiChannel::ShortwaveIr).is_none(),
        "non-triviality: something WAS evicted, so the pin above is what kept \
         longwave rather than there having been room for everything"
    );
}

#[test]
fn a_refetch_of_a_resident_channel_replaces_its_own_key() {
    let mut cache = GmgsiGridCache::new(4 * 200);
    cache.insert(
        GmgsiChannel::LongwaveIr,
        granule_of(GmgsiChannel::LongwaveIr, 100),
        &[],
    );
    cache.insert(
        GmgsiChannel::LongwaveIr,
        granule_of(GmgsiChannel::LongwaveIr, 100),
        &[],
    );
    assert_eq!(
        cache.len(),
        1,
        "a refetch is a replacement, not a second entry"
    );
    assert_eq!(cache.resident_bytes(), 400);
}

/// The shipped budget must hold at least one real channel, or the layer can
/// never draw anything at all.
///
/// The `>=` half of this is already a `const _: () = assert!(..)` in the
/// handler, so repeating it here would be an assertion clippy can see is
/// constant and a reader cannot see is redundant. What is left is the figure
/// itself: 3000 x 5000 x 4 bytes, which is the number every budget arm is a
/// multiple of.
#[test]
fn one_global_mosaic_is_sixty_megabytes() {
    assert_eq!(GLOBAL_GRID_BYTES, 60_000_000);
    assert_eq!(
        GRID_CACHE_BYTES % GLOBAL_GRID_BYTES,
        0,
        "the budget is spent in whole channels"
    );
}

// -- Registration -----------------------------------------------------------

#[test]
fn the_layer_declares_the_weight_and_the_clock_it_was_registered_with() {
    let h = GmgsiHandler::new();
    assert_eq!(h.id(), known::GMGSI);
    assert_eq!(
        h.draw_order_weight(),
        5,
        "below the model at 10 and MRMS at 15: satellite is the backdrop"
    );
    assert_eq!(
        h.auto_poll_interval(),
        Some(600),
        "the blend lands 34 to 42 minutes after the hour it covers"
    );
    assert!(
        matches!(h.time_axis(), rustdar_source::time::TimeAxis::Live),
        "a third FrameSeries layer needs a ruling; this one is Live",
    );
    assert_eq!(h.render_mode(), RenderMode::Texture);
    assert_eq!(
        h.job_codec().map(|row| row.label),
        Some("overlay/model"),
        "this layer rides the gridded row and adds no codec of its own"
    );
    assert_eq!(h.surface(), Surface::Ground);
    assert_eq!(h.display_name(), "Global Satellite");
}

/// Four channels, one group, each with a paint — and the discontinued fifth
/// product absent.
#[test]
fn the_four_channels_are_this_layers_products() {
    let h = GmgsiHandler::new();
    assert_eq!(h.products().len(), 4);
    for spec in h.products() {
        assert_eq!(spec.group, crate::gmgsi::fields::GROUP);
        assert!(
            crate::render::gridded::field_paint(&spec.id).is_some(),
            "{:?} is offered by the layer but paints nothing",
            spec.id,
        );
    }
    assert!(
        !h.products().iter().any(|s| s.code.contains("Ssr")),
        "GMGSI_SSR was discontinued 2025-06-03 and must not be registered"
    );
}

/// **This pane's own channel**, not the registry copy's: the pane holds water
/// vapour where the handler's own default is longwave.
#[test]
fn the_current_field_is_this_panes_own_channel_as_the_registry_spells_it() {
    let h = handler_with(GmgsiChannel::LongwaveIr, vec![82.0; 4]);
    let state = pane_state(GmgsiChannel::WaterVapor);
    let pane = PaneRef {
        state: Some(&*state),
        ..PaneRef::bare(0)
    };
    let field = h.current_field(&pane).expect("a layer with products");
    assert_eq!(
        field,
        crate::gmgsi::fields::spec(GmgsiChannel::WaterVapor)
            .id
            .clone(),
    );
    assert!(h.products().iter().any(|spec| spec.id == field));
}

/// A hidden layer still names the field it *would* draw, as the model and MRMS
/// do: the catalogue tile of a switched-off layer is how a user turns it on at
/// the field they wanted.
#[test]
fn a_hidden_pane_still_names_the_field_it_would_draw() {
    let h = GmgsiHandler::new();
    assert!(!h.is_enabled(&PaneRef::across(&[])));
    assert_eq!(
        h.current_field(&PaneRef::across(&[])),
        Some(
            crate::gmgsi::fields::spec(GmgsiChannel::LongwaveIr)
                .id
                .clone()
        ),
    );
}

// -- The Resident carry -----------------------------------------------------

/// Describing a job must cost a refcount. `Arc::ptr_eq` is the assertion —
/// equality would pass on a clone of the 60 MB.
#[test]
fn prepare_job_describes_the_resident_grid_without_copying_it() {
    let h = handler_with(GmgsiChannel::LongwaveIr, vec![82.0; 8]);
    let pane = PaneRef::across(&[]);
    let resident = Arc::clone(
        &h.cached_grids
            .get(GmgsiChannel::LongwaveIr)
            .expect("the arrival is resident")
            .grid,
    );
    let job = h
        .prepare_job(&rctx(), &pane)
        .expect("a resident channel describes a job");
    let input = job
        .downcast_ref::<rasterize::GriddedInput>()
        .expect("the gridded carry, which is what lets this layer share the row");
    let rasterize::GriddedInput::Resident(carried) = input else {
        panic!("GMGSI must describe a Resident carry, not {input:?}");
    };
    assert!(
        Arc::ptr_eq(carried, &resident),
        "the job carried a copy of the raster rather than a refcount"
    );
}

#[test]
fn a_pane_with_no_resident_mosaic_describes_no_job() {
    let h = GmgsiHandler::new();
    assert!(h.prepare_job(&rctx(), &PaneRef::across(&[])).is_none());
}

fn rctx() -> RasterizeContext {
    let clock = chrono::Utc::now().naive_utc();
    RasterizeContext {
        is_dark: true,
        zoom: 4.0,
        device_scale: 1.0,
        now: clock,
        as_of: clock,
        frame: None,
    }
}

// -- Reopen is 1:1 ----------------------------------------------------------

#[test]
fn every_pane_state_field_survives_a_save_and_a_reload() {
    let h = GmgsiHandler::new();
    for enabled in [true, false] {
        for &channel in GmgsiChannel::all() {
            let saved = GmgsiPaneState {
                enabled,
                selected_channel: channel,
            };
            let json = h.serialize_pane_state(&saved as &dyn std::any::Any);
            let restored = h
                .deserialize_pane_state(json, !enabled)
                .expect("this layer keeps pane state");
            let restored = restored
                .downcast_ref::<GmgsiPaneState>()
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
    let h = GmgsiHandler::new();
    let restored = h
        .deserialize_pane_state(serde_json::json!({}), true)
        .expect("pane state");
    let restored = restored.downcast_ref::<GmgsiPaneState>().unwrap();
    assert!(
        restored.enabled,
        "the slot flag stands in for a missing key"
    );
    assert_eq!(restored.selected_channel, GmgsiChannel::LongwaveIr);
}

/// A channel spelling this build does not know is ignored, not defaulted into
/// some other channel's data — through both doors it can arrive by.
#[test]
fn an_unknown_channel_id_is_ignored() {
    let h = GmgsiHandler::new();
    let restored = h
        .deserialize_pane_state(
            serde_json::json!({"enabled": true, "channel": "GmgsiSsr"}),
            false,
        )
        .expect("pane state");
    let restored = restored.downcast_ref::<GmgsiPaneState>().unwrap();
    assert_eq!(restored.selected_channel, GmgsiChannel::LongwaveIr);

    let mut h = handler_with(GmgsiChannel::LongwaveIr, vec![82.0; 4]);
    let effect = h.apply_control(
        &ControlUpdate {
            id: "channel",
            value: ControlValue::String("GmgsiSsr".into()),
        },
        &mut PaneMut::bare(0),
    );
    assert!(matches!(effect, ControlEffect::None));
    assert_eq!(h.defaults.selected_channel, GmgsiChannel::LongwaveIr);
}

// -- Controls ---------------------------------------------------------------

/// Switching to a **resident** channel needs no network; switching to one that
/// is not resident does.
#[test]
fn switching_channels_fetches_only_what_is_not_already_in_hand() {
    let mut h = handler_with(GmgsiChannel::LongwaveIr, vec![82.0; 4]);
    let before = h.state.data_generation;

    let effect = h.apply_control(
        &ControlUpdate {
            id: "channel",
            value: ControlValue::String(GmgsiChannel::WaterVapor.as_str().into()),
        },
        &mut PaneMut::bare(0),
    );
    assert!(
        matches!(effect, ControlEffect::Fetch),
        "water vapour is not resident, so the pane must ask for it",
    );

    let effect = h.apply_control(
        &ControlUpdate {
            id: "channel",
            value: ControlValue::String(GmgsiChannel::LongwaveIr.as_str().into()),
        },
        &mut PaneMut::bare(0),
    );
    assert!(
        matches!(effect, ControlEffect::None),
        "longwave is still resident, so switching back must redraw rather \
         than refetch",
    );
    assert_ne!(
        h.state.data_generation, before,
        "the redraw needs a new content signature, or the pane keeps its old \
         texture",
    );
}

/// The dropdown's option values are exactly the ids the registry publishes, so
/// a catalogue tile's `FieldId` goes straight through `apply_control`.
#[test]
fn the_channel_dropdown_offers_the_registered_field_ids() {
    let h = handler_with(GmgsiChannel::LongwaveIr, vec![82.0; 4]);
    let options: Vec<String> = h
        .controls(&PaneRef::bare(0))
        .into_iter()
        .find_map(|item| match item {
            ControlItem::Dropdown {
                id: "channel",
                options,
                ..
            } => Some(options.into_iter().map(|(v, _)| v.to_string()).collect()),
            _ => None,
        })
        .expect("the channel dropdown");
    let registered: Vec<String> = h
        .products()
        .iter()
        .map(|spec| spec.id.as_str().to_string())
        .collect();
    assert_eq!(options, registered);
    assert_eq!(h.field_control_id(), Some("channel"));
}

/// The toggle names the granule's own **valid** time, not the fetch time: the
/// blend lands ~40 minutes after the hour it covers, so those are different
/// facts and the first is the one on screen.
#[test]
fn the_toggle_label_carries_the_granules_valid_time() {
    let toggle = |h: &GmgsiHandler| {
        h.controls(&PaneRef::bare(0))
            .into_iter()
            .find_map(|item| match item {
                ControlItem::Toggle { label, .. } => Some(label),
                _ => None,
            })
            .expect("a toggle")
    };
    let h = handler_with(GmgsiChannel::LongwaveIr, vec![82.0; 4]);
    assert_eq!(toggle(&h), "Global Satellite (12:00z)");
    assert_eq!(
        toggle(&GmgsiHandler::new()),
        "Global Satellite",
        "no data, no time to claim"
    );
}

// -- Cross-pane -------------------------------------------------------------

/// Two panes on two channels get two content signatures, or the render dispatch
/// groups them and draws one raster for both.
#[test]
fn two_panes_on_two_channels_do_not_share_a_content_signature() {
    let h = handler_with(GmgsiChannel::LongwaveIr, vec![82.0; 4]);
    let a = pane_state(GmgsiChannel::LongwaveIr);
    let b = pane_state(GmgsiChannel::WaterVapor);
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
fn an_arrival_pins_every_panes_channel_not_just_its_own() {
    let h = handler_with(GmgsiChannel::LongwaveIr, vec![82.0; 4]);
    let a = pane_state(GmgsiChannel::LongwaveIr);
    let b = pane_state(GmgsiChannel::Visible);
    let states: Vec<&dyn std::any::Any> = vec![&*a, &*b];
    let pinned = h.pinned_channels(&PaneRef::across(&states));
    assert!(pinned.contains(&GmgsiChannel::LongwaveIr));
    assert!(pinned.contains(&GmgsiChannel::Visible));
    assert_eq!(pinned.len(), 2, "deduplicated, and both panes counted");
}

// -- The sentinels, at the layer's own surface ------------------------------

/// **What the NaN mapping buys at this layer**: a `_FillValue` point answers no
/// tooltip at all, rather than "Longwave IR: -9999 count".
#[test]
fn a_fill_value_point_reports_no_reading() {
    let mut values = vec![82.0f32; 8];
    values[4] = f32::NAN;
    let h = handler_with(GmgsiChannel::LongwaveIr, values);
    let pane = PaneRef::across(&[]);
    // Column 4 of the fixture axis: -99.0 + 4 * 0.072.
    assert_eq!(h.hover_value_at(35.0, -99.0 + 4.0 * 0.072, &pane), None);
    // Its neighbour is ordinary data and does answer, so the None above is the
    // fill and not a hover that never works.
    assert_eq!(
        h.hover_value_at(35.0, -99.0 + 3.0 * 0.072, &pane),
        Some("Longwave IR: 82 count".to_string()),
    );
}

/// A hover far outside the mosaic answers nothing rather than the nearest edge
/// cell half a world away.
#[test]
fn a_point_outside_the_mosaic_reports_no_reading() {
    let h = handler_with(GmgsiChannel::LongwaveIr, vec![82.0; 8]);
    assert_eq!(h.hover_value_at(-60.0, 140.0, &PaneRef::across(&[])), None);
}
