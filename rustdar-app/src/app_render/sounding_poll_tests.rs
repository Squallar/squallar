use super::stamping_tests::{SITE, app_showing_site};
use rustdar_radar::sounding::EnvHeights;

fn heights(h0c_km_msl: f64) -> EnvHeights {
    EnvHeights {
        h0c_km_msl,
        hm20c_km_msl: h0c_km_msl + 3.2,
        fetched_at: chrono::Utc::now(),
    }
}

/// As the sounding spawn in `spawn_level3_fetches` produces one.
fn landed(generation: u64, heights: Option<EnvHeights>) -> crate::channels::SoundingResponse {
    crate::channels::SoundingResponse {
        generation,
        site: SITE.to_string(),
        heights,
    }
}

/// A landed sounding is stored per site, and a failed refetch keeps the previous entry
/// rather than clearing it: stale environmental heights beat none, and it is precisely the
/// entry *staying stale* that makes the TTL gate retry on the next poll.
#[test]
fn a_failed_refetch_keeps_the_previous_heights() {
    let mut app = app_showing_site();
    app.channels
        .sounding_sender
        .send(landed(0, Some(heights(4.2))))
        .unwrap();
    app.poll_level3_results();
    assert_eq!(
        app.render.env_heights.get(SITE).map(|h| h.h0c_km_msl),
        Some(4.2),
        "the landed sounding never reached env_heights",
    );

    app.channels.sounding_sender.send(landed(0, None)).unwrap();
    app.poll_level3_results();
    assert_eq!(
        app.render.env_heights.get(SITE).map(|h| h.h0c_km_msl),
        Some(4.2),
        "a failed refetch cleared the stored heights instead of keeping them",
    );
}

/// The per-site fetch-generation gate covers soundings too: a result from a superseded
/// fetch must not land.
#[test]
fn a_superseded_sounding_result_is_discarded() {
    let mut app = app_showing_site();
    let superseded = app.render.next_fetch_generation(SITE);
    app.render.next_fetch_generation(SITE);

    app.channels
        .sounding_sender
        .send(landed(superseded, Some(heights(9.9))))
        .unwrap();
    app.poll_level3_results();

    assert!(
        !app.render.env_heights.contains_key(SITE),
        "a sounding from a superseded fetch generation was stored",
    );
}
