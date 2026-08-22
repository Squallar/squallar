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
    assert_eq!(
        h.time_axis(),
        rustdar_source::time::TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(3600),
            extends_future: false,
        },
        "the hourly blend, stopping at the wall clock",
    );
    assert_eq!(
        h.min_loop_frames(),
        13,
        "thirteen hourly mosaics, matching what the model layer declares"
    );
    assert_eq!(
        h.min_loop_span_secs(),
        43_200,
        "twelve hours: thirteen frames is twelve hourly steps end to end"
    );
    assert_eq!(
        h.frame_horizon(&PaneRef::across(&[])),
        chrono::Duration::zero(),
        "a mosaic exists for an hour that has happened and for no other"
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

// -- The frame contract (WB-11) ---------------------------------------------

/// Midnight of the fixture's day, so every hour below is `hour(k)`.
fn hour(k: i64) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2025, 6, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        + chrono::Duration::hours(k)
}

/// A granule for `hour(k)` whose **values name the hour**, so two frames' grids
/// can never be mistaken for one another however they are compared.
fn granule_at(channel: GmgsiChannel, k: i64, n: usize) -> GmgsiGrid {
    let mut g = grid_of(channel, vec![k as f32; n]);
    g.valid_time = hour(k);
    g
}

/// The object key a listing would have found for `hour(k)` — unpredictable in
/// production (`_c` is the creation stamp), arbitrary here.
fn object_key(channel: GmgsiChannel, k: i64) -> String {
    format!(
        "{}/{}/{}_v3r0_blend_s{}00000_e{}09599_c{}34579.nc",
        channel.prefix(),
        hour(k).format("%Y/%m/%d/%H"),
        channel.object_stem(),
        hour(k).format("%Y%m%d%H"),
        hour(k).format("%Y%m%d%H"),
        hour(k).format("%Y%m%d%H"),
    )
}

/// Hand the handler the listing its own `create_frame_list_task` would have
/// produced for hours `0..count`, on the one production door.
fn file_listing(h: &mut GmgsiHandler, channel: GmgsiChannel, count: i64, complete: bool) {
    let range = (hour(0), hour(count - 1));
    let keys: Vec<(chrono::NaiveDateTime, String)> = (0..count)
        .map(|k| (hour(k), object_key(channel, k)))
        .collect();
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
        Box::new(GmgsiListing {
            channel,
            range,
            keys,
            complete,
        }),
        &PaneRef::across(&[]),
    );
}

/// Deliver one frame's granule on the one production door.
fn file_frame(h: &mut GmgsiHandler, channel: GmgsiChannel, k: i64, n: usize) {
    h.apply_frame(
        FrameStamp {
            valid: hour(k),
            run: None,
        },
        Box::new(GmgsiFrameFetch {
            channel,
            valid: hour(k),
            grid: Some(granule_at(channel, k, n)),
        }),
        &PaneRef::across(&[]),
    );
}

fn frame_ctx(k: i64) -> RasterizeContext {
    RasterizeContext {
        frame: Some(FrameStamp {
            valid: hour(k),
            run: None,
        }),
        ..rctx()
    }
}

/// The values the job describes, so a raster's identity can be read without
/// comparing 60 MB by hand.
fn job_values(job: &DescribedJob) -> Vec<f32> {
    let input = job
        .downcast_ref::<rasterize::GriddedInput>()
        .expect("the gridded carry");
    let rasterize::GriddedInput::Resident(grid) = input else {
        panic!("GMGSI must describe a Resident carry, not {input:?}");
    };
    grid.values.clone()
}

/// A `FetchConfig` with nothing behind it: every test here stops before the
/// network, and the two methods that take one read only `client`/`sources` to
/// clone into a future nothing polls.
///
/// `tls::client` and not `reqwest::Client::new()`: the bare constructor panics
/// for want of a crypto provider, and it only *happens* to work in a whole-lib
/// run because some earlier test installed one. That is a filtered run reading
/// green off another test's side effect.
fn fetch_config() -> FetchConfig {
    FetchConfig {
        client: rustdar_source::tls::client(
            rustdar_source::tls::USER_AGENT,
            std::time::Duration::from_secs(1),
        )
        .build()
        .expect("a client with a crypto provider installed"),
        zone_cache_dir: None,
        sources: rustdar_source::origins::DataSources::default(),
        viewport: None,
        as_of: chrono::Utc::now().naive_utc(),
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
    let channel = GmgsiChannel::LongwaveIr;
    // The live picture is a granule NO frame shares, so a fallback to it is
    // visible rather than coincidentally right.
    let mut h = handler_with(channel, vec![7.0; 8]);
    file_listing(&mut h, channel, 13, true);
    // A staging area with room for both, so what is asserted is the LOOKUP and
    // not the eviction policy, which has its own test below.
    h.frame_grids = GmgsiFrameCache::new(4 * 8 * 4);
    file_frame(&mut h, channel, 3, 8);
    file_frame(&mut h, channel, 9, 8);

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
        "the frame at 03z was drawn from another hour's granule"
    );
    assert_eq!(
        at(9),
        vec![9.0f32; 8],
        "the frame at 09z was drawn from another hour's granule"
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
        vec![7.0f32; 8],
    );

    // And a listed frame with no granule staged describes NOTHING, rather than
    // handing this hour the live picture under another hour's label.
    assert!(
        h.prepare_job(&frame_ctx(5), &pane).is_none(),
        "an unstaged frame fell back to the pane's picture: one hour's \
         satellite image presented, unlabelled, as another's"
    );
}

/// **Residency does not grow with the frame count.**
///
/// Thirteen frames are listed, thirteen granules are delivered one after
/// another exactly as the serialised fetch delivers them, and the layer never
/// holds more than the staging budget buys — one mosaic.
///
/// The byte arithmetic, with its denominator: **one mosaic is
/// `3000 * 5000 * 4` = 60,000,000 B (57.22 MiB)**, so thirteen resident would
/// be 780,000,000 B — against a 96 MiB wasm model pool and a 56 MiB wasm loop
/// pool, 14x and 15x over. The loop's own storage is thirteen *textures* at
/// 11.06 MB for a 1280x960-point pane, which is a different budget in a
/// different crate.
///
/// **Floor — `stage_every_frame`:** delete the eviction loop in
/// `GmgsiFrameCache::insert`.
#[test]
fn the_layer_stages_one_granule_however_many_frames_the_loop_holds() {
    let channel = GmgsiChannel::LongwaveIr;
    // Room for exactly one 8-value grid: the shipped ratio, at a size a test
    // can reach.
    let mut h = GmgsiHandler::with_frame_budget(4 * 8);
    h.defaults.enabled = true;
    h.defaults.selected_channel = channel;
    file_listing(&mut h, channel, 13, true);

    let pane = PaneRef::across(&[]);
    let ctx = fetch_config();
    assert_eq!(
        h.list_frames(&ctx, &pane, (hour(0), hour(12))).frames.len(),
        13,
        "premise: the listing must have named thirteen frames, or there is no \
         frame count for residency to fail to track"
    );

    for k in 0..13 {
        file_frame(&mut h, channel, k, 8);
        assert_eq!(
            h.frame_grids.len(),
            1,
            "after {} granules the layer holds {}. One mosaic is \
             3000 x 5000 x 4 = {GLOBAL_GRID_BYTES} B, so {} resident is {} B, \
             against a 96 MiB model pool and a 56 MiB loop pool on wasm. A \
             loop holds textures, not grids.",
            k + 1,
            h.frame_grids.len(),
            h.frame_grids.len(),
            h.frame_grids.len() * GLOBAL_GRID_BYTES,
        );
        assert_eq!(
            h.frames_resident(&pane),
            vec![FrameStamp {
                valid: hour(k),
                run: None
            }],
            "the one staged granule must be the one that just landed"
        );
    }

    // The shipped arithmetic the fixture stands in for.
    assert_eq!(
        FRAME_STAGING_BYTES, GLOBAL_GRID_BYTES,
        "one mosaic stages at a time on every arm"
    );
    assert_eq!(
        13 * GLOBAL_GRID_BYTES,
        780_000_000,
        "thirteen resident granules, spelled out"
    );
}

/// **One granule at a time is enough for the pipeline to advance**, which is
/// the claim the staging budget rests on.
///
/// Each arrival is described into a job before the next lands — the order the
/// pump imposes, since the fetches are serialised and the pump runs between
/// them — and the job keeps its own refcount, so the picture survives the
/// eviction the very next arrival causes.
///
/// **Floor — `describe_after_the_flood`:** move the `prepare_job` calls below
/// the loop that delivers all thirteen granules.
#[test]
fn a_granule_evicted_after_its_job_is_described_still_paints_that_frame() {
    let channel = GmgsiChannel::LongwaveIr;
    let mut h = GmgsiHandler::with_frame_budget(4 * 8);
    h.defaults.enabled = true;
    h.defaults.selected_channel = channel;
    file_listing(&mut h, channel, 13, true);
    let pane = PaneRef::across(&[]);

    let mut described: Vec<Vec<f32>> = Vec::new();
    for k in 0..13 {
        file_frame(&mut h, channel, k, 8);
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
    let channel = GmgsiChannel::LongwaveIr;
    let mut h = GmgsiHandler::with_frame_budget(4 * 8);
    h.defaults.enabled = true;
    h.defaults.selected_channel = channel;
    let pane = PaneRef::across(&[]);
    let ctx = fetch_config();
    let ask = |h: &GmgsiHandler, k: i64| {
        h.fetch_frame(
            &ctx,
            &pane,
            &FrameStamp {
                valid: hour(k),
                run: None,
            },
        )
        .is_some()
    };

    assert!(
        !ask(&h, 3),
        "nothing has been listed, so there is no key to GET and no way to \
         invent one"
    );
    file_listing(&mut h, channel, 13, true);
    assert!(ask(&h, 3), "a listed hour is one GET away");
    assert!(
        !ask(&h, 99),
        "an hour outside every listing is declined rather than guessed at"
    );
    file_frame(&mut h, channel, 3, 8);
    assert!(!ask(&h, 3), "a staged granule is not fetched twice");
}

/// The listing's cost is **1 LIST per hour**, and it is bounded: past
/// `MAX_FRAME_LIST_REQUESTS` the hours are sampled with both ends kept, so the
/// window still spans the same ground.
///
/// The widest window the Lookback slider can name is 1440 minutes, which is 25
/// hours, so the bound does not bind on anything a user can ask for today —
/// which is exactly why it is asserted against a range no slider produces.
#[test]
fn a_frame_listing_costs_one_request_per_hour_and_is_bounded() {
    use crate::gmgsi::fetch::{MAX_FRAME_LIST_REQUESTS, hours_in_range};

    // The floor window: thirteen frames, thirteen requests.
    let twelve_hours = hours_in_range((hour(0), hour(12)));
    assert_eq!(twelve_hours.len(), 13);
    assert_eq!(twelve_hours.first(), Some(&hour(0)));
    assert_eq!(twelve_hours.last(), Some(&hour(12)));

    // The slider's own ceiling, which must not be sampled — and the constant
    // half in a const block, where a violated bound is a build failure.
    assert_eq!(hours_in_range((hour(0), hour(24))).len(), 25);
    const _: () = assert!(25 <= MAX_FRAME_LIST_REQUESTS);

    // A window nothing can ask for today: bounded, and both ends kept.
    let wide = hours_in_range((hour(0), hour(400)));
    assert_eq!(
        wide.len(),
        MAX_FRAME_LIST_REQUESTS,
        "a 401-hour window would be 401 LIST requests unbounded"
    );
    assert_eq!(wide.first(), Some(&hour(0)));
    assert_eq!(wide.last(), Some(&hour(400)));
    assert!(
        wide.windows(2).all(|w| w[0] < w[1]),
        "the sampled hours must stay in order and hold no duplicate"
    );

    // Partial hours at either end name granules outside the window.
    assert_eq!(
        hours_in_range((hour(0) + chrono::Duration::minutes(1), hour(2))),
        vec![hour(1), hour(2)],
        "an hour whose granule is stamped before the window's start is not in it"
    );
    assert!(hours_in_range((hour(5), hour(4))).is_empty());
}

/// `list_frames` says `complete` only where a listing that really covered the
/// window landed. A sampled or failed one leaves the answer readable as "at
/// least these", which is what it is.
#[test]
fn an_incomplete_listing_never_settles_the_window() {
    let channel = GmgsiChannel::LongwaveIr;
    let mut h = GmgsiHandler::new();
    h.defaults.enabled = true;
    h.defaults.selected_channel = channel;
    let pane = PaneRef::across(&[]);
    let ctx = fetch_config();

    file_listing(&mut h, channel, 13, false);
    let listed = h.list_frames(&ctx, &pane, (hour(0), hour(12)));
    assert_eq!(listed.frames.len(), 13, "the keys are filed either way");
    assert!(
        !listed.complete,
        "an incomplete listing must not claim the window is settled"
    );

    file_listing(&mut h, channel, 13, true);
    assert!(h.list_frames(&ctx, &pane, (hour(0), hour(12))).complete);
    // A window wider than anything covered is still open.
    assert!(!h.list_frames(&ctx, &pane, (hour(0), hour(20))).complete);
}

/// Frames are scoped to the pane's own channel: another band's granule is not
/// a frame this pane can draw, and offering it would paint the wrong band.
#[test]
fn one_channels_frames_are_not_offered_to_a_pane_on_another() {
    let mut h = GmgsiHandler::with_frame_budget(4 * 8 * 4);
    h.defaults.enabled = true;
    h.defaults.selected_channel = GmgsiChannel::LongwaveIr;
    file_listing(&mut h, GmgsiChannel::LongwaveIr, 13, true);
    file_listing(&mut h, GmgsiChannel::WaterVapor, 13, true);
    file_frame(&mut h, GmgsiChannel::LongwaveIr, 3, 8);
    file_frame(&mut h, GmgsiChannel::WaterVapor, 4, 8);

    let lw = pane_state(GmgsiChannel::LongwaveIr);
    let wv = pane_state(GmgsiChannel::WaterVapor);
    let pane_lw = PaneRef {
        state: Some(&*lw),
        ..PaneRef::bare(0)
    };
    let pane_wv = PaneRef {
        state: Some(&*wv),
        ..PaneRef::bare(1)
    };
    assert_eq!(
        h.frames_resident(&pane_lw),
        vec![FrameStamp {
            valid: hour(3),
            run: None
        }],
    );
    assert_eq!(
        h.frames_resident(&pane_wv),
        vec![FrameStamp {
            valid: hour(4),
            run: None
        }],
    );
    assert_eq!(
        h.frame_grids.len(),
        2,
        "both are staged; what differs is which pane is offered which"
    );
}

/// `retain_frames` is the eviction door, and it drops this pane's channel
/// alone. **Nothing above calls it yet**, which is why it is driven here.
#[test]
fn retain_frames_drops_this_channels_unkept_granules_and_no_others() {
    let mut h = GmgsiHandler::with_frame_budget(4 * 8 * 8);
    h.defaults.enabled = true;
    h.defaults.selected_channel = GmgsiChannel::LongwaveIr;
    file_listing(&mut h, GmgsiChannel::LongwaveIr, 13, true);
    for k in 0..3 {
        file_frame(&mut h, GmgsiChannel::LongwaveIr, k, 8);
    }
    file_frame(&mut h, GmgsiChannel::WaterVapor, 7, 8);
    assert_eq!(h.frame_grids.len(), 4, "premise: four granules are staged");

    h.retain_frames(
        &PaneRef::across(&[]),
        &[FrameStamp {
            valid: hour(1),
            run: None,
        }],
    );
    assert_eq!(
        h.frames_resident(&PaneRef::across(&[])),
        vec![FrameStamp {
            valid: hour(1),
            run: None
        }],
    );
    assert_eq!(
        h.frame_grids.len(),
        2,
        "the other channel's granule belongs to another pane and is untouched"
    );
}

/// **The gate serialises, and it releases.** [`GmgsiHandler::fetch_frame`]'s
/// doc claims the whole render set may be dispatched at once while only one
/// granule is ever in flight; this is the floor under that claim — the cache
/// half ("one granule *resident*") has its own test above, and without this
/// one the in-flight half had none: removing the gate left every suite green
/// while thirteen concurrent fetches would hold thirteen 60 MB decodes
/// (~780 MB) before any cache saw one.
///
/// Two fetch tasks are driven by hand inside one thread — `futures::poll!`
/// with `yield_now` turns between, so "the second gets N chances" is a poll
/// count and never a wall clock — against a loopback server that records
/// every request line and **holds hour 0's response open** until told:
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
/// `let _one_at_a_time = ();`. Observed red at step 2: both request lines
/// recorded while hour 0 was still held open.
#[test]
fn two_frame_fetches_share_one_gate_and_the_second_waits_for_the_first() {
    use std::io::{Read, Write};
    use std::sync::Mutex;

    // -- A loopback bucket that records request lines and holds hour 0 -----
    // Not `glm`'s `s3_recording`: that mock answers each request before
    // reading the next, and a serialisation test needs the first response
    // withheld while a second connection is accepted — so each connection
    // gets its own thread, and hour 0's waits on the channel.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    let seen: Arc<Mutex<Vec<String>>> = Default::default();
    let (release, held) = std::sync::mpsc::channel::<()>();
    let held = Arc::new(Mutex::new(held));
    let recorder = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let recorder = Arc::clone(&recorder);
            let held = Arc::clone(&held);
            std::thread::spawn(move || {
                let mut scratch = [0u8; 4096];
                let read = stream.read(&mut scratch).unwrap_or(0);
                let request = String::from_utf8_lossy(&scratch[..read]).to_string();
                let line = request.lines().next().unwrap_or("").to_string();
                let hold = line.contains("/2025/06/01/00/");
                recorder
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(line);
                if hold {
                    let _ = held.lock().unwrap_or_else(|e| e.into_inner()).recv();
                }
                let body = "not a granule; the decode failing is fine here";
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len(),
                    )
                    .as_bytes(),
                );
            });
        }
    });

    let channel = GmgsiChannel::LongwaveIr;
    let mut h = GmgsiHandler::new();
    h.defaults.enabled = true;
    h.defaults.selected_channel = channel;
    file_listing(&mut h, channel, 2, true);

    // `tls::client` sets `https_only`, which a loopback URL cannot satisfy;
    // `tls::init` is still required because reqwest is pinned to
    // `rustls-no-provider`. Timeout wide enough that a loaded machine cannot
    // time the held response out mid-test.
    rustdar_source::tls::init();
    let cfg = FetchConfig {
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("a cleartext loopback client"),
        zone_cache_dir: None,
        sources: rustdar_source::origins::DataSources {
            gmgsi_bucket: "gmgsi".into(),
            s3_base: format!("http://127.0.0.1:{port}/{{bucket}}").into(),
            ..rustdar_source::origins::DataSources::production()
        },
        viewport: None,
        as_of: chrono::Utc::now().naive_utc(),
    };
    let pane = PaneRef::across(&[]);
    let mut a = h
        .fetch_frame(
            &cfg,
            &pane,
            &FrameStamp {
                valid: hour(0),
                run: None,
            },
        )
        .expect("hour 0 is listed")
        .future;
    let mut b = h
        .fetch_frame(
            &cfg,
            &pane,
            &FrameStamp {
                valid: hour(1),
                run: None,
            },
        )
        .expect("hour 1 is listed")
        .future;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    rt.block_on(async move {
        let gets = |seen: &Arc<Mutex<Vec<String>>>| -> usize {
            seen.lock().unwrap_or_else(|e| e.into_inner()).len()
        };

        // 1. A acquires the gate and puts its GET on the wire, where the
        // server holds it. The cap is a poll count, not a clock.
        let mut turns = 0usize;
        while gets(&seen) == 0 {
            assert!(turns < 200_000, "the first fetch never issued its GET");
            assert!(
                futures::poll!(a.as_mut()).is_pending(),
                "hour 0 completed while the server was holding its response",
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
             still in flight ({in_flight} request lines recorded). Unserialised, \
             a thirteen-frame render set holds thirteen 60 MB decodes at once, \
             ~780 MB in flight before any cache can evict anything.",
        );

        // 3. Release the held response; A completes and drops the guard.
        release.send(()).expect("the server thread is alive");
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
            lines[0].contains("/2025/06/01/00/") && lines[1].contains("/2025/06/01/01/"),
            "the two GETs must be the two listed keys in dispatch order: {lines:?}",
        );
    });
}
