use super::*;

/// The real IEM index for the instant the Moore EF5 was on the ground. Captured
/// rather than hand-written: the fields this reads are IEM's to name, and a
/// fixture invented here would pin what I assumed the service sends.
const MOORE_INDEX: &str = include_str!("moore_2013_mcd.json");

/// The body of MD 0728, as IEM serves it — the discussion covering central into
/// north-eastern Oklahoma while the tornado was down.
const MD_0728: &str = include_str!("md0728.txt");

/// The instant the tornado was on the ground — the reference the `VALID`
/// line's bare day-of-month fields resolve against.
fn moore_instant() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2013, 5, 20)
        .unwrap()
        .and_hms_opt(19, 59, 0)
        .unwrap()
}

fn index() -> serde_json::Value {
    serde_json::from_str(MOORE_INDEX).expect("the captured IEM index parses")
}

/// Both discussions in force at the pinned instant are found, with the numbers
/// SPC gave them.
#[test]
fn the_captured_index_yields_both_discussions() {
    let entries = index_entries(&index());
    assert_eq!(entries.len(), 2, "got {entries:?}");

    let numbers: Vec<u32> = entries.iter().map(|(_, _, n)| *n).collect();
    assert!(
        numbers.contains(&727) && numbers.contains(&728),
        "{numbers:?}"
    );

    for (product_id, year, _) in &entries {
        assert_eq!(*year, 2013);
        assert!(
            product_id.contains("SWOMCD"),
            "{product_id} is not a mesoscale discussion product"
        );
    }
}

/// An index row missing any of the three fields is skipped rather than panicking
/// or being half-built.
///
/// A `product_id` with no `num` cannot be titled and a `num` with no
/// `product_id` cannot be fetched; either way the row is unusable, and the
/// alternative to skipping it is an MD drawn as "MD 0000".
#[test]
fn an_incomplete_index_row_is_skipped() {
    let mut json = index();
    json["features"][0]["properties"]
        .as_object_mut()
        .expect("properties is an object")
        .remove("num");
    assert_eq!(index_entries(&json).len(), 1, "the complete row survives");

    assert!(index_entries(&serde_json::json!({})).is_empty());
    assert!(index_entries(&serde_json::json!({"features": []})).is_empty());
}

/// The archived body goes through the same derivation as a live one: the
/// polygon comes from its `LAT...LON` block and the type from its prose.
///
/// This is the whole reason the archive fetches product text rather than
/// reading IEM's GeoJSON geometry. A discussion whose summary leads with
/// tornadoes must come out Convective — and therefore the same colour — no
/// matter which service delivered it.
#[test]
fn an_archived_body_derives_the_same_discussion_a_live_one_would() {
    let md = discussion_from_text(
        728,
        "Mesoscale Discussion 728".into(),
        "https://www.spc.noaa.gov/products/md/2013/md0728.html".into(),
        MD_0728.to_string(),
        moore_instant(),
    )
    .expect("MD 0728 is displayable");

    assert_eq!(md.number, 728);
    assert_eq!(
        md.md_type,
        crate::spc::discussion::MdType::Convective,
        "a tornado-threat discussion is convective, and its colour follows"
    );
    assert_eq!(
        md.concerning.as_deref(),
        Some("TORNADO WATCH 190...191..."),
        "the concerning line is what the popup heads with"
    );

    let ring = md.polygon.first().expect("MD 0728 carries a polygon");
    assert!(
        ring.len() >= 4,
        "a closed ring needs at least four points, got {}",
        ring.len()
    );
    for (lat, lon) in ring {
        assert!(
            (30.0..=40.0).contains(lat) && (-102.0..=-92.0).contains(lon),
            "({lat}, {lon}) is not over Oklahoma/Kansas — the LAT...LON decode moved"
        );
    }
}

/// The `VALID` line becomes the window the as-of filter reads.
///
/// MD 0728 says `VALID 201931Z - 202130Z`, so it is in force at 19:59 on the
/// 20th — the instant the volume behind it was scanned — and gone by 22:00.
/// Without this the layer would draw every archived discussion at every
/// instant, which is the failure it was drawing today's discussions with.
#[test]
fn the_valid_line_becomes_the_window() {
    use crate::spc::discussion::parse_valid_window;
    let (from, until) = parse_valid_window(MD_0728, moore_instant());
    let at = |d, h, m| {
        chrono::NaiveDate::from_ymd_opt(2013, 5, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    };
    assert_eq!(from, Some(at(20, 19, 31)));
    assert_eq!(until, Some(at(20, 21, 30)));

    let valid_at = |t| from.is_none_or(|f| f <= t) && until.is_none_or(|u| t < u);
    assert!(valid_at(moore_instant()), "in force at the pinned volume");
    assert!(
        !valid_at(at(20, 19, 0)),
        "not in force before it was issued"
    );
    assert!(!valid_at(at(20, 22, 0)), "not in force after it expired");
}

/// The day-of-month field resolves across a month boundary, in both directions.
///
/// The product names `DDHHMM` and nothing else, so the month comes from the
/// instant being asked about. A discussion issued late on the 31st and read
/// just after midnight on the 1st must resolve to the month it was issued in,
/// not to the 31st of the month being read — which would put it a month in the
/// future and make it never in force.
#[test]
fn a_bare_day_of_month_resolves_against_the_instant_being_asked_about() {
    use crate::spc::discussion::parse_valid_window;
    let dt = |y, mo, d, h, m| {
        chrono::NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    };

    // Issued 23:40Z on 31 July, read at 00:10Z on 1 August.
    let (from, _) = parse_valid_window("VALID 312340Z - 010200Z", dt(2024, 8, 1, 0, 10));
    assert_eq!(
        from,
        Some(dt(2024, 7, 31, 23, 40)),
        "the reference month is not the issue month"
    );

    // And the reverse: issued 00:10Z on the 1st, read at 01:00Z on the 1st,
    // where the naive "same month" answer happens to be right.
    let (from, _) = parse_valid_window("VALID 010010Z - 010400Z", dt(2024, 8, 1, 1, 0));
    assert_eq!(from, Some(dt(2024, 8, 1, 0, 10)));

    // A day that does not exist in the neighbouring month is not chosen.
    let (from, _) = parse_valid_window("VALID 310100Z - 310300Z", dt(2024, 3, 31, 2, 0));
    assert_eq!(from, Some(dt(2024, 3, 31, 1, 0)), "February has no 31st");
}

/// A product with no `VALID` line, or a malformed one, is drawn rather than
/// hidden.
///
/// `None` on a side passes the filter on that side. The alternative — treating
/// an unparseable window as "never in force" — makes one bad product invisible
/// instead of merely unbounded, and does it silently.
#[test]
fn an_unparseable_window_leaves_the_discussion_unbounded() {
    use crate::spc::discussion::parse_valid_window;
    let now = moore_instant();
    assert_eq!(parse_valid_window("no such line here", now), (None, None));
    assert_eq!(
        parse_valid_window("VALID whenever - later", now),
        (None, None)
    );
    // A parseable start with a junk end keeps the start.
    let (from, until) = parse_valid_window("VALID 201931Z - junk", now);
    assert!(from.is_some() && until.is_none(), "{from:?} {until:?}");
}

/// The archive URL names the instant, in the spelling IEM's API accepts.
#[test]
fn the_index_url_addresses_the_instant() {
    let sources = DataSources::production();
    let at = chrono::NaiveDate::from_ymd_opt(2013, 5, 20)
        .unwrap()
        .and_hms_opt(19, 59, 0)
        .unwrap();
    let url = sources.spc_discussions_archive_url(at);
    assert!(url.contains("valid=2013-05-20T19:59:00Z"), "{url}");
    assert!(url.contains("spc_mcd.geojson"), "{url}");
}

/// The archive reads IEM without a `User-Agent`.
///
/// IEM answers `OPTIONS` with `405` and no `Access-Control-Allow-Methods`, so a
/// `User-Agent` turns this into a preflighted request that the browser never
/// sends — silently, and on web only. The application-wide client carries one,
/// which is why this path builds its own.
#[test]
fn the_archive_client_sends_no_user_agent() {
    let client = DataSources::production()
        .iem_client(ARCHIVE_TIMEOUT)
        .build()
        .expect("the IEM client must build");
    assert!(
        !squallar_source::tls::sends_user_agent(&client),
        "the archived-MD client carries a User-Agent, so the browser preflights \
         the GET, IEM answers OPTIONS with 405, and a scrubbed pane silently \
         shows no discussions — on web and only on web"
    );
}

/// Counterweight to the test above: a client that ignored the recorded rule
/// would otherwise pass it, because production says "no User-Agent" anyway.
#[test]
fn the_archive_client_follows_the_origins_recorded_rule() {
    let sources = DataSources {
        metar_sends_user_agent: true,
        ..DataSources::production()
    };
    let client = sources
        .iem_client(ARCHIVE_TIMEOUT)
        .build()
        .expect("the IEM client must build");
    assert!(
        squallar_source::tls::sends_user_agent(&client),
        "iem_client ignores DataSources::metar_sends_user_agent"
    );
}

/// The link an archived discussion offers is SPC's own page for it.
#[test]
fn the_popup_link_points_at_the_spc_page_for_that_discussion() {
    let url = spc_archive_link(&DataSources::production(), 2013, 728);
    assert_eq!(url, "https://www.spc.noaa.gov/products/md/2013/md0728.html");
}

// ── The routing: which service a pane's fetch actually reaches ─────────────
//
// The fetcher above is only useful if the handler dispatches to it, and a
// handler that never routed would leave every test above green. `FetchTask`
// carries an opaque future, so this drives the future and reads which code path
// answered out of the error text: every leg of every fetch names itself in its
// own message, and the two legs' spellings differ.
//
// Native-only: the runtime is `tokio::rt-multi-thread`, a native dev-dependency.
#[cfg(not(target_arch = "wasm32"))]
mod routing {
    use crate::render::handlers::discussion::SpcDiscussionFetchResult;
    use crate::render::overlay_state::{OverlayHandler, PaneRef};
    use squallar_source::handler::FetchConfig;

    /// Every origin points at a host that resolves nowhere, so no request
    /// leaves the machine and both legs fail in their own words.
    ///
    /// `.invalid` is reserved by RFC 2606 §2 precisely so it can never be
    /// registered; the clients are `https_only`, so a loopback stub would need
    /// a certificate to be reachable at all.
    fn nowhere() -> squallar_source::origins::DataSources {
        squallar_source::origins::DataSources {
            iem_base: "https://iem.squallar-test.invalid".into(),
            spc_base: "https://spc.squallar-test.invalid".into(),
            ..squallar_source::origins::DataSources::production()
        }
    }

    fn ctx_at(as_of: chrono::NaiveDateTime) -> FetchConfig {
        squallar_source::tls::init();
        FetchConfig {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("client"),
            zone_cache_dir: None,
            sources: nowhere(),
            viewport: None,
            as_of,
            depicted_span_secs: None,
            depicted_frames: Vec::new(),
        }
    }

    /// The message from whichever leg ran.
    fn leg(as_of: chrono::NaiveDateTime) -> String {
        let handler = crate::render::handlers::discussion::SpcDiscussionHandler::new();
        let ctx = ctx_at(as_of);
        let mut tasks = handler.create_fetch_tasks(&ctx, &PaneRef::bare(0));
        assert_eq!(tasks.len(), 1, "one discussion fetch per round");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let payload = runtime.block_on(tasks.remove(0).future);
        let result = payload
            .downcast::<SpcDiscussionFetchResult>()
            .expect("a discussion round");
        match result.0 {
            Ok(v) => panic!(
                "a fetch from nowhere succeeded with {} discussions",
                v.len()
            ),
            Err(e) => format!("{e:?}"),
        }
    }

    /// A pane scrubbed to a past storm reaches the ARCHIVE.
    ///
    /// This is the whole point: before it did, the layer fetched the standing
    /// feed and drew today's discussions over a decade-old volume.
    #[test]
    fn a_parked_pane_reaches_the_archive() {
        let moore = chrono::NaiveDate::from_ymd_opt(2013, 5, 20)
            .unwrap()
            .and_hms_opt(19, 59, 0)
            .unwrap();
        let msg = leg(moore);
        assert!(
            msg.contains("archived MD index"),
            "a pane parked at the Moore EF5 did not reach the archive: {msg}"
        );
    }

    /// A live pane still reaches the standing RSS feed.
    ///
    /// The counterweight. Without it a handler that routed EVERY pane to the
    /// archive would pass the test above, and live panes would quietly lose the
    /// discussions issued in the last half hour.
    #[test]
    fn a_live_pane_still_reaches_the_standing_feed() {
        let msg = leg(chrono::Utc::now().naive_utc());
        assert!(
            msg.contains("SPC MD RSS"),
            "a live pane was routed away from the standing feed: {msg}"
        );
    }
}
