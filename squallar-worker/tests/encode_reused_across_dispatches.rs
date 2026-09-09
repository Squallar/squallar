//! **That a pan re-encodes nothing whose input has not moved.**
//!
//! `JobRequest::to_parts` runs at the dispatch site, on the frame thread, and
//! writes the row's whole message. For the polygon layers that message is the
//! geometry, written two `f64`s at a time. `prepare_job` memoises the built
//! input behind a stable `Arc`, but the *envelope* carries the viewport, so a
//! gesture that moves only the camera handed back the same input and encoded
//! it again for every dispatch.
//!
//! **The window is a gesture, not an idle pane.** An idle pane dispatches
//! nothing at all — `squallar-app`'s
//! `an_idle_pane_asks_for_no_further_rasters_once_its_layers_hold_a_picture`
//! is the pin on that, and a census over 350 settled frames at 175 Hz recorded
//! zero encodes. What repeats is the gesture: over two loops of the tree's own
//! `pan-zoom-2d` script, 7,000 frames, the alerts row encoded 27 times from
//! **one** input and 26 of those wrote bytes identical to the first.
//!
//! Every count below is a delta over `encode_cache::totals`, which is
//! thread-local — so these tests do not read each other's rows whether libtest
//! gives them a thread each or runs them on one.

#![cfg(not(target_arch = "wasm32"))]

use squallar_source::job::JobGeometry;
use squallar_worker::encode_cache::{self, EncodeTotals};
use squallar_worker::offload::JobRequest;

/// The envelope's own width on the wire: `[code u8][w u32][h u32]`
/// `[min_lat,max_lat,min_lon,max_lon f64][side_ceiling_px u32]`, which is
/// `JobRequest::write_envelope` field for field. The row's own bytes — the
/// only part any of this remembers — start after it.
const ENVELOPE: usize = 1 + 4 + 4 + 8 * 4 + 4;

/// A viewport. `n` moves the ground the way a pan does, leaving the texture
/// size alone.
fn a_viewport(n: f64) -> JobGeometry {
    JobGeometry {
        width: 1248,
        height: 714,
        bounds: squallar_geo::GeoBounds {
            min_lat: 30.0 + n * 0.05,
            max_lat: 40.0 + n * 0.05,
            min_lon: -100.0 + n * 0.05,
            max_lon: -90.0 + n * 0.05,
        },
        side_ceiling_px: 0,
    }
}

/// An alerts input at the scale the live feed delivers: 455 alerts, each one
/// zone-shaped ring of 290 vertices. The same fixture
/// `job_head_allocated_once` prices its head from, and for the same reason —
/// `api.weather.gov/alerts/active` answered 455 alerts on 2026-09-08 and the
/// app's transport ledger reads 1.78-2.47 MiB per request once the UGC zones
/// are resolved to rings.
fn an_alerts_input() -> squallar_overlays::render::rasterize::AlertsInput {
    use squallar_overlays::render::rasterize::{AlertPaint, AlertsInput};
    use squallar_source::feature::{HatchPattern, OverlayFeature};

    let alerts = (0..455)
        .map(|z| {
            let ring: Vec<(f64, f64)> = (0..290)
                .map(|v| {
                    let t = v as f64 / 290.0 * std::f64::consts::TAU;
                    (35.0 + z as f64 * 0.01 + t.sin(), -97.0 + t.cos())
                })
                .collect();
            AlertPaint {
                id: format!("urn:oid:2.49.0.1.840.0.{z}"),
                category: squallar_overlays::nws::alert::AlertCategory::Warning,
                features: std::sync::Arc::new(vec![OverlayFeature::new(
                    vec![vec![ring]],
                    [255, 0, 0, 96],
                    [255, 0, 0, 255],
                    "TOR".to_string(),
                    "Tornado Warning".to_string(),
                    HatchPattern::None,
                )]),
            }
        })
        .collect();
    AlertsInput {
        alerts,
        enabled_categories: squallar_overlays::nws::alert::AlertCategory::ALL.to_vec(),
        hidden_ids: Default::default(),
        device_scale: 2.0,
    }
}

fn delta(before: EncodeTotals, after: EncodeTotals) -> (u64, u64) {
    (after.encoded - before.encoded, after.reused - before.reused)
}

/// **The claim.** One input dispatched at twelve viewports runs the encoder
/// **once**, and every message carries byte-identical row bytes under a
/// different envelope.
///
/// Floor — the arm that fails in the other direction: `encoded` must be
/// exactly 1, never 0. A cache that answered every dispatch without ever
/// encoding anything would satisfy "no repeat encodes" and post empty
/// messages, so the first encode is asserted to have happened and its bytes
/// to be the full head.
///
/// Tamper — take `ENCODE_IGNORES_CTX` off `AlertsJob`, or make
/// `encode_cache::encode_row` always call the encoder: `encoded` reads 12.
#[test]
fn a_pan_encodes_one_unchanged_polygon_set_once() {
    const DISPATCHES: usize = 12;
    // **One `DescribedJob`, twelve requests.** This is the whole shape of the
    // thing: `prepare_job` hands the dispatch a refcount clone of the same
    // input while the data and the filters have not moved, and only the
    // envelope's viewport differs between the messages. Describing the input
    // afresh each time — a different `Arc` — is a different input as far as
    // anything here is concerned, and is what the storm-reports row really
    // does.
    let described = squallar_source::job::DescribedJob::new(an_alerts_input());
    let mut messages: Vec<Vec<u8>> = Vec::new();

    let before = encode_cache::totals();
    for n in 0..DISPATCHES {
        let request = JobRequest {
            job: described.clone(),
            geometry: a_viewport(n as f64),
        };
        messages.push(request.to_bytes());
    }
    let (encoded, reused) = delta(before, encode_cache::totals());

    assert_eq!(
        encoded, 1,
        "twelve dispatches of one unchanged polygon set ran the encoder \
         {encoded} time(s); it must run exactly once. Zero would mean nothing \
         was ever encoded and every message below is empty.",
    );
    assert_eq!(
        reused,
        DISPATCHES as u64 - 1,
        "{reused} of the {} later dispatches were served a remembered encode",
        DISPATCHES - 1,
    );
    assert!(
        messages[0].len() > ENVELOPE + encode_cache::FLOOR_BYTES,
        "floor: the fixture encoded to {} B, which is not the megabyte head \
         this is about — every count above would be about nothing",
        messages[0].len(),
    );
    for (n, message) in messages.iter().enumerate().skip(1) {
        assert_eq!(
            &message[ENVELOPE..],
            &messages[0][ENVELOPE..],
            "dispatch {n} carried different row bytes from the first",
        );
        assert_ne!(
            &message[..ENVELOPE],
            &messages[0][..ENVELOPE],
            "dispatch {n} carried the same envelope as the first, so the \
             viewport never moved and this window is not the pan it claims",
        );
    }
}

/// **Byte-identity, both spellings.** A remembered encode has to produce the
/// message the encoder would have, not merely a message.
///
/// Tamper — have `encode_cache::reuse` append the bytes twice: `warm` and
/// `cold` differ.
#[test]
fn a_remembered_encode_is_byte_identical_to_the_one_it_replaces() {
    let described = squallar_source::job::DescribedJob::new(an_alerts_input());
    let geo = a_viewport(3.0);
    let request = || JobRequest {
        job: described.clone(),
        geometry: geo,
    };

    // Cold: nothing is held for this input, so the encoder runs.
    let before = encode_cache::totals();
    let cold = request().to_bytes();
    let (encoded, _) = delta(before, encode_cache::totals());
    assert_eq!(encoded, 1, "the cold window did not run the encoder");

    // Warm: the same input, the same viewport.
    let before = encode_cache::totals();
    let warm = request().to_bytes();
    let (encoded, reused) = delta(before, encode_cache::totals());
    assert_eq!((encoded, reused), (0, 1), "the warm window re-encoded");
    assert_eq!(warm, cold, "the remembered encode is not the same message");

    // And the message still decodes to a job this build runs.
    assert!(
        JobRequest::from_bytes(&warm).is_some(),
        "the remembered encode produced a message this build cannot decode",
    );

    // Cleared: the encoder runs again and writes the same bytes, so what was
    // remembered was the encoder's own output and not something derived once.
    encode_cache::clear();
    let before = encode_cache::totals();
    let again = request().to_bytes();
    let (encoded, _) = delta(before, encode_cache::totals());
    assert_eq!(
        encoded, 1,
        "a cleared table did not fall back to the encoder"
    );
    assert_eq!(again, cold, "the re-encode differs from the first encode");
}

/// **A row that reads the viewport is never served a remembered encode.**
///
/// `GriddedJob` cuts its payload to the texture bounds at encode time — it is
/// the whole reason `EncodeCtx` exists — and leaves `ENCODE_IGNORES_CTX` at
/// its safe default. Two dispatches of one grid at two viewports must run the
/// encoder twice and write different bytes.
///
/// Tamper — set `ENCODE_IGNORES_CTX = true` on `GriddedJob`: `reused` reads 1
/// and the two windows' bytes become equal, which is the wrong picture this
/// default exists to prevent.
#[test]
fn a_row_that_reads_the_viewport_is_encoded_again_for_every_one() {
    // One described grid at two viewports, so the ONLY thing standing between
    // the second dispatch and a remembered encode is the row's own default.
    let described = squallar_source::job::DescribedJob::new(a_gridded_input());
    let at = |n: f64| {
        JobRequest {
            job: described.clone(),
            geometry: a_viewport(n),
        }
        .to_bytes()
    };

    let before = encode_cache::totals();
    let a = at(0.0);
    let b = at(40.0);
    let (encoded, reused) = delta(before, encode_cache::totals());

    assert!(
        a.len() - ENVELOPE > encode_cache::FLOOR_BYTES,
        "floor: this grid's window encodes to {} B, under the {} B the table \
         files at all — the row would be passed over for its SIZE rather than \
         for reading the viewport, and opting it in would not change a thing",
        a.len() - ENVELOPE,
        encode_cache::FLOOR_BYTES,
    );

    assert_eq!(
        (encoded, reused),
        (2, 0),
        "the gridded row was served a remembered encode; it cuts its payload \
         to the viewport, so a reused one is the wrong ground rasterized",
    );
    assert_ne!(
        &a[ENVELOPE..],
        &b[ENVELOPE..],
        "floor: the two viewports produced identical row bytes, so this \
         fixture would pass even if the row WERE served a cached encode",
    );
}

/// **An entry holds its input.** An `Arc` whose last owner drops frees its
/// allocation and the allocator may hand that address to the next input, so a
/// table that remembered only the address would answer a hit for a value it
/// never saw. Measured, not hypothetical: a census keeping only the address
/// read 10 false matches in 32 dispatches of the storm-reports row, and 0 once
/// it held the inputs.
///
/// Tamper — store the pointer instead of the `DescribedJob` in
/// `encode_cache::Entry`: the strong count does not rise.
#[test]
fn a_remembered_encode_keeps_its_input_alive() {
    let input = an_alerts_input();
    let described = squallar_source::job::DescribedJob::new(input);
    let before = std::sync::Arc::strong_count(&described.0);

    // **The request is dropped before the count is read.** It holds a clone of
    // its own, so a count taken while it is alive rises whether the table kept
    // anything or not — which is a pass this test cannot be allowed to have.
    let message = {
        let request = JobRequest {
            job: described.clone(),
            geometry: a_viewport(7.0),
        };
        request.to_bytes()
    };
    assert!(
        message.len() > ENVELOPE + encode_cache::FLOOR_BYTES,
        "floor: this input encoded to {} B, under the size the table files at \
         all, so the count below would be about an entry that was never made",
        message.len(),
    );

    assert!(
        std::sync::Arc::strong_count(&described.0) > before,
        "the table filed this input's encode without holding the input: the \
         strong count is {} and was {before} before the dispatch. A freed \
         allocation can be handed to the next input at the same address, and \
         this entry would then answer for it.",
        std::sync::Arc::strong_count(&described.0),
    );
}

/// **The table is bounded and evicts.** An unbounded encode table on a 1 GiB
/// wasm heap is a residency defect traded for a latency win.
///
/// Tamper — remove the eviction loop in `encode_cache::remember`: `held_bytes`
/// runs past the cap.
#[test]
fn the_table_never_holds_more_than_its_cap() {
    // Enough distinct inputs that keeping them all would be several times the
    // cap; each is its own `Arc`, so none of them can hit.
    let mut kept = Vec::new();
    for n in 0..8 {
        let mut input = an_alerts_input();
        input.device_scale = 1.0 + n as f32 * 0.25;
        let request = JobRequest::describe(input, a_viewport(n as f64));
        let message = request.to_bytes();
        assert!(
            message.len() > encode_cache::FLOOR_BYTES,
            "floor: input {n} is under the filing floor and never entered the \
             table, so the cap below was never tested",
        );
        kept.push(message.len());
    }
    let held = encode_cache::totals().held_bytes;
    let would_be: usize = kept.iter().sum();
    assert!(
        would_be > encode_cache::CAP_BYTES,
        "floor: all eight encodes together are {would_be} B, inside the \
         {} B cap — nothing was ever evicted and this test asserts nothing",
        encode_cache::CAP_BYTES,
    );
    assert!(
        held <= encode_cache::CAP_BYTES,
        "the table is holding {held} B against a {} B cap",
        encode_cache::CAP_BYTES,
    );
}

/// A model grid, the one input in the workspace whose encode is cut to the
/// viewport.
fn a_gridded_input() -> squallar_overlays::render::rasterize::GriddedInput {
    use squallar_overlays::hrrr::GridCoords;
    use squallar_overlays::render::gridded::{GridValues, ResidentGrid, ScaledU16};

    // Sized so the window a `a_viewport` cut leaves is comfortably past
    // `encode_cache::FLOOR_BYTES`; a smaller grid would be passed over for its
    // size and prove nothing about the context default.
    let (ni, nj) = (2400usize, 2400usize);
    let field = squallar_overlays::render::gridded::paint_for_code("vis")
        .expect("this build registers the `vis` field")
        .id
        .clone();
    squallar_overlays::render::rasterize::GriddedInput::Resident(std::sync::Arc::new(
        ResidentGrid {
            field,
            ni,
            nj,
            coords: GridCoords::Regular {
                lat0: 30.0,
                lon0: -100.0,
                dlat: 0.005,
                dlon: 0.005,
                ni,
                nj,
                scan_mode: 0,
            },
            values: GridValues::Scaled(ScaledU16 {
                codes: (0..(ni * nj) as u16).collect(),
                ref_val: 0.0,
                two_pow: 1.0,
                dig_factor: 1.0,
                nan_codes: vec![],
            }),
        },
    ))
}
