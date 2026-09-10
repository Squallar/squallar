use super::stamping_tests::{SITE, app_showing_site};
use squallar_radar::sounding::EnvHeights;

/// The hour these fixtures speak for. Fixed rather than `now()` so the lookup
/// below asks for the same hour the sample was filed under.
fn hour() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-07-28T18:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}

fn heights(h0c_km_msl: f64) -> EnvHeights {
    EnvHeights {
        h0c_km_msl,
        hm20c_km_msl: h0c_km_msl + 3.2,
        valid_at: hour(),
        fetched_at: hour(),
    }
}

/// The 0 °C height the app holds for this site at [`hour`].
fn held(app: &crate::app::App) -> Option<f64> {
    app.render
        .env_heights
        .get(SITE)
        .and_then(|store| store.at(hour()))
        .map(|(h0c, _)| h0c)
}

/// As the sounding spawn in `spawn_level3_fetches` produces one.
fn landed(generation: u64, heights: Option<EnvHeights>) -> crate::channels::SoundingResponse {
    crate::channels::SoundingResponse {
        generation,
        site: SITE.to_string(),
        heights: heights.into_iter().collect(),
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
        held(&app),
        Some(4.2),
        "the landed sounding never reached env_heights",
    );

    app.channels.sounding_sender.send(landed(0, None)).unwrap();
    app.poll_level3_results();
    assert_eq!(
        held(&app),
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
