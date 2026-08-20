use super::*;

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
}

// -- Identifier ---------------------------------------------------------

#[test]
fn identifier_splits_site_and_collection_time() {
    let id = Identifier::new("KTLX20240520_000004_V06".to_string());
    assert_eq!(id.name(), "KTLX20240520_000004_V06");
    assert_eq!(id.site(), Some("KTLX"));
    let dt = id.date_time().expect("parses");
    assert_eq!(
        dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        "2024-05-20 00:00:04"
    );
}

#[test]
fn identifier_rejects_names_it_cannot_slice() {
    for name in ["", "KTL", "KTLX", "KTLX2024"] {
        let id = Identifier::new(name.to_string());
        assert!(
            id.date_time().is_none(),
            "{name:?} should have no parseable date/time"
        );
    }
}

// -- key -> name --------------------------------------------------------

#[test]
fn key_to_identifier_drops_exactly_the_date_and_site_segments() {
    let id = key_to_identifier("2024/05/20/KTLX/KTLX20240520_000004_V06");
    assert_eq!(id.name(), "KTLX20240520_000004_V06");
}

#[test]
fn key_to_identifier_keeps_mdm_sidecars() {
    let id = key_to_identifier("2024/05/20/KTLX/KTLX20240520_000004_V06_MDM");
    assert_eq!(id.name(), "KTLX20240520_000004_V06_MDM");
}

// -- URLs ---------------------------------------------------------------

#[test]
fn day_prefix_is_zero_padded_and_date_partitioned() {
    assert_eq!(day_prefix("KTLX", &date(2024, 5, 6)), "2024/05/06/KTLX");
    assert_eq!(day_prefix("KDMX", &date(2011, 11, 27)), "2011/11/27/KDMX");
}

#[test]
fn list_url_is_a_v2_prefix_query() {
    let url = list_url(
        "https://bkt.s3.amazonaws.com",
        "2024/05/20/KTLX",
        None,
        None,
    )
    .expect("url");
    assert!(url.starts_with("https://bkt.s3.amazonaws.com/?"), "{url}");
    assert!(url.contains("list-type=2"), "{url}");
    assert!(url.contains("prefix=2024%2F05%2F20%2FKTLX"), "{url}");
    assert!(
        !url.contains("continuation-token"),
        "first page must not carry a cursor: {url}"
    );
}

#[test]
fn list_url_carries_max_keys_only_when_set() {
    let bare = list_url("https://bkt.s3.amazonaws.com", "p", None, None).expect("url");
    assert!(!bare.contains("max-keys"), "{bare}");
    let capped = list_url("https://bkt.s3.amazonaws.com", "p", Some(7), None).expect("url");
    assert!(capped.contains("max-keys=7"), "{capped}");
}

#[test]
fn list_url_percent_encodes_the_continuation_token() {
    let token = "abc/def+ghi=";
    let url = list_url("https://bkt.s3.amazonaws.com", "p", None, Some(token)).expect("url");
    assert!(
        url.contains("continuation-token=abc%2Fdef%2Bghi%3D"),
        "token not encoded into the query: {url}"
    );
    assert!(
        !url.contains("abc/def+ghi="),
        "token appears unencoded: {url}"
    );
}

#[test]
fn object_url_is_the_key_under_the_bucket_host() {
    assert_eq!(
        crate::sources::DataSources::production()
            .s3_object_url("bkt", "2024/05/20/KTLX/KTLX20240520_000004_V06"),
        "https://bkt.s3.amazonaws.com/2024/05/20/KTLX/KTLX20240520_000004_V06"
    );
}

// -- status classification ---------------------------------------------

#[test]
fn only_200_is_treated_as_a_complete_body() {
    assert_eq!(classify(StatusCode::OK), StatusClass::Ok);
    assert_eq!(classify(StatusCode::NOT_FOUND), StatusClass::NotFound);
    for status in [
        StatusCode::PARTIAL_CONTENT,
        StatusCode::NO_CONTENT,
        StatusCode::NOT_MODIFIED,
        StatusCode::FORBIDDEN,
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        assert_eq!(
            classify(status),
            StatusClass::Failed,
            "{status} must not be read as a body or as an absence"
        );
    }
}

// -- XML parsing --------------------------------------------------------

/// Build a `ListBucketResult` document.
fn listing(keys: &[&str], next_token: Option<&str>) -> String {
    let contents: String = keys
        .iter()
        .map(|k| {
            format!(
                "<Contents><Key>{k}</Key>\
                     <LastModified>2025-07-15T22:47:45.000Z</LastModified>\
                     <Size>8717892</Size><StorageClass>STANDARD</StorageClass></Contents>"
            )
        })
        .collect();
    let truncated = next_token.is_some();
    let token = next_token
        .map(|t| format!("<NextContinuationToken>{t}</NextContinuationToken>"))
        .unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
<Name>unidata-nexrad-level2</Name><Prefix>2024/05/20/KTLX</Prefix>
{token}<KeyCount>{}</KeyCount><MaxKeys>1000</MaxKeys>
<IsTruncated>{truncated}</IsTruncated>{contents}</ListBucketResult>"#,
        keys.len()
    )
}

#[test]
fn parse_list_page_reads_keys_flag_and_cursor() {
    let page =
        parse_list_page(&listing(&["a/b/c/d/one", "a/b/c/d/two"], Some("TOK"))).expect("parses");
    assert_eq!(page.keys, vec!["a/b/c/d/one", "a/b/c/d/two"]);
    assert!(page.truncated);
    assert_eq!(page.next_token.as_deref(), Some("TOK"));
}

#[test]
fn parse_list_page_reads_a_final_page() {
    let page = parse_list_page(&listing(&["a/b/c/d/one"], None)).expect("parses");
    assert_eq!(page.keys, vec!["a/b/c/d/one"]);
    assert!(!page.truncated);
    assert_eq!(page.next_token, None);
}

#[test]
fn parse_list_page_only_takes_keys_inside_contents() {
    let doc = listing(&["a/b/c/d/real"], None).replacen(
        "<Contents>",
        "<CommonPrefixes><Prefix>2024/05/20/KTLX/</Prefix></CommonPrefixes>\
             <Error><Code>NoSuchKey</Code><Key>a/b/c/d/phantom</Key></Error>\
             <Contents>",
        1,
    );
    let page = parse_list_page(&doc).expect("parses");
    assert_eq!(
        page.keys,
        vec!["a/b/c/d/real"],
        "captured a Key from outside Contents"
    );
    assert_eq!(
        page.common_prefixes,
        vec!["2024/05/20/KTLX/"],
        "the CommonPrefixes entry belongs in its own bucket, not discarded \
             and not among the keys"
    );
}

#[test]
fn parse_list_page_only_takes_prefixes_inside_common_prefixes() {
    let doc = listing(&[], None);
    assert!(
        doc.contains("<Prefix>2024/05/20/KTLX</Prefix>"),
        "the fixture must carry a top-level Prefix or this proves nothing"
    );
    let page = parse_list_page(&doc).expect("parses");
    assert!(
        page.common_prefixes.is_empty(),
        "the document's own echoed Prefix was mistaken for a directory: {:?}",
        page.common_prefixes
    );
}

#[test]
fn parse_list_page_reads_a_delimited_listing() {
    let doc = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
<Name>unidata-nexrad-level2-chunks</Name><Prefix>KTLX/</Prefix>
<Delimiter>/</Delimiter><IsTruncated>false</IsTruncated>
<CommonPrefixes><Prefix>KTLX/1/</Prefix></CommonPrefixes>
<CommonPrefixes><Prefix>KTLX/10/</Prefix></CommonPrefixes>
<CommonPrefixes><Prefix>KTLX/2/</Prefix></CommonPrefixes>
</ListBucketResult>"#;
    let page = parse_list_page(doc).expect("parses");
    assert!(page.keys.is_empty());
    assert_eq!(
        page.common_prefixes,
        vec!["KTLX/1/", "KTLX/10/", "KTLX/2/"],
        "and in S3's UTF-8 order, which is not numeric — the caller has to \
             parse and sort"
    );
}

#[test]
fn list_url_delimited_asks_for_directories() {
    let url =
        list_url_delimited("https://bucket.s3.amazonaws.com", "KTLX/", "/", None).expect("url");
    assert!(url.contains("delimiter=%2F"), "{url}");
    assert!(url.contains("prefix=KTLX%2F"), "{url}");

    let plain = list_url("https://bucket.s3.amazonaws.com", "KTLX/", None, None).expect("url");
    assert!(
        !plain.contains("delimiter"),
        "an undelimited listing must stay undelimited: {plain}"
    );
}

#[test]
fn parse_list_page_reads_an_empty_listing() {
    let page = parse_list_page(&listing(&[], None)).expect("parses");
    assert!(page.keys.is_empty());
    assert!(!page.truncated);
}

// -- pagination ---------------------------------------------------------

/// Drive `collect_keys` over canned pages, recording the URLs requested.
fn paginate(
    pages: Vec<std::result::Result<String, ArchiveError>>,
) -> (Result<Vec<String>>, Vec<String>) {
    let urls = std::cell::RefCell::new(Vec::new());
    let remaining = std::cell::RefCell::new(std::collections::VecDeque::from(pages));

    let sources = DataSources::production();
    let bucket_url = sources.s3_bucket_url(&sources.level2_bucket);
    let outcome = {
        let fut = collect_keys(&bucket_url, "2024/05/20/KTLX", Some(2), |url| {
            urls.borrow_mut().push(url);
            let next = remaining
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Err(ArchiveError::MalformedListing("no more pages".into())));
            async move { next }
        });
        let mut fut = Box::pin(fut);
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        match fut.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(v) => v,
            std::task::Poll::Pending => panic!("fixture fetcher never yields"),
        }
    };

    (outcome, urls.into_inner())
}

#[test]
fn pagination_follows_the_cursor_across_every_page() {
    let (keys, urls) = paginate(vec![
        Ok(listing(&["a/b/c/d/k1", "a/b/c/d/k2"], Some("TOK1"))),
        Ok(listing(&["a/b/c/d/k3", "a/b/c/d/k4"], Some("TOK2"))),
        Ok(listing(&["a/b/c/d/k5"], None)),
    ]);
    let keys = keys.expect("listing should complete");

    assert_eq!(
        keys,
        vec![
            "a/b/c/d/k1",
            "a/b/c/d/k2",
            "a/b/c/d/k3",
            "a/b/c/d/k4",
            "a/b/c/d/k5",
        ],
        "paged listing lost or reordered keys"
    );
    assert_eq!(urls.len(), 3, "expected one request per page");
    assert!(
        !urls[0].contains("continuation-token"),
        "first request must not carry a cursor: {}",
        urls[0]
    );
    assert!(
        urls[1].contains("continuation-token=TOK1"),
        "second request did not carry the first page's cursor: {}",
        urls[1]
    );
    assert!(
        urls[2].contains("continuation-token=TOK2"),
        "third request did not carry the second page's cursor: {}",
        urls[2]
    );
}

#[test]
fn pagination_stops_on_the_truncation_flag_not_the_cursor() {
    let body = listing(&["a/b/c/d/k1"], None).replace(
        "</ListBucketResult>",
        "<NextContinuationToken>STALE</NextContinuationToken></ListBucketResult>",
    );
    let (keys, urls) = paginate(vec![Ok(body)]);
    assert_eq!(keys.expect("completes"), vec!["a/b/c/d/k1"]);
    assert_eq!(urls.len(), 1, "followed a cursor on an untruncated page");
}

#[test]
fn pagination_refuses_to_truncate_silently() {
    let body = listing(&["a/b/c/d/k1"], None).replace(
        "<IsTruncated>false</IsTruncated>",
        "<IsTruncated>true</IsTruncated>",
    );
    let (keys, urls) = paginate(vec![Ok(body)]);
    let err = keys.expect_err("must not return a partial listing as success");
    assert!(
        matches!(err, ArchiveError::MalformedListing(_)),
        "unexpected error: {err:?}"
    );
    assert_eq!(urls.len(), 1);
}

#[test]
fn pagination_gives_up_after_the_page_cap() {
    let pages = (0..MAX_LIST_PAGES + 5)
        .map(|i| Ok(listing(&["a/b/c/d/k"], Some(&format!("TOK{i}")))))
        .collect();
    let (keys, urls) = paginate(pages);
    let err = keys.expect_err("an endless listing must terminate as an error");
    assert!(
        matches!(err, ArchiveError::MalformedListing(_)),
        "unexpected error: {err:?}"
    );
    assert_eq!(urls.len(), MAX_LIST_PAGES, "page cap not enforced");
}

#[test]
fn pagination_propagates_a_page_failure() {
    let (keys, urls) = paginate(vec![
        Ok(listing(&["a/b/c/d/k1"], Some("TOK1"))),
        Err(ArchiveError::Status {
            status: StatusCode::SERVICE_UNAVAILABLE,
            url: "https://example.invalid/".into(),
            body: None,
        }),
    ]);
    let err = keys.expect_err("a failed page must not be reported as the end of the listing");
    assert!(matches!(err, ArchiveError::Status { .. }), "{err:?}");
    assert_eq!(urls.len(), 2);
}

// -- response handling --------------------------------------------------
//
// `classify` is covered above, but nothing there proves `get_text` still
// *consults* it. These drive the real reqwest path against a one-shot
// loopback server: hermetic, but a genuine HTTP round trip.

/// Serve exactly one canned HTTP response and return the URL to hit.
fn serve_once(response: String) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        if let Ok((mut stream, _)) = listener.accept() {
            let mut scratch = [0u8; 4096];
            let _ = stream.read(&mut scratch);
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://127.0.0.1:{port}/")
}

fn http_response(status_line: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/xml\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn loopback_client() -> reqwest::Client {
    crate::tls::init();
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("client")
}

#[tokio::test]
async fn get_text_returns_the_body_on_200() {
    let url = serve_once(http_response("200 OK", "<ListBucketResult/>"));
    let body = get_text(&loopback_client(), url)
        .await
        .expect("200 should yield a body");
    assert_eq!(body, "<ListBucketResult/>");
}

/// A `503` is an error, not an empty listing.
#[tokio::test]
async fn get_text_reports_a_server_error_instead_of_an_empty_listing() {
    let url = serve_once(http_response(
        "503 Service Unavailable",
        "<Error><Code>SlowDown</Code></Error>",
    ));
    let err = get_text(&loopback_client(), url)
        .await
        .expect_err("a 503 body must not be returned as a listing");
    match err {
        ArchiveError::Status { status, .. } => {
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        }
        other => panic!("expected Status, got {other:?}"),
    }
}

#[tokio::test]
async fn get_text_reports_a_404_as_not_found() {
    let url = serve_once(http_response(
        "404 Not Found",
        "<Error><Code>NoSuchKey</Code></Error>",
    ));
    let err = get_text(&loopback_client(), url)
        .await
        .expect_err("a 404 must not be returned as a body");
    assert!(
        matches!(err, ArchiveError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

// -- live ---------------------------------------------------------------
//
// Run with:
//   cargo test -p rustdar-radar --lib -- --ignored --nocapture archive::tests::live_

/// End-to-end against the real bucket.
#[ignore = "hits the live unidata-nexrad-level2 S3 bucket"]
#[tokio::test]
async fn live_archive_lists_downloads_and_decodes_a_volume() {
    let sources = DataSources::production();
    let day = date(2024, 5, 20);
    let files = list_files(&sources, "KTLX", &day)
        .await
        .expect("listing KTLX 2024-05-20");
    println!("listed {} objects", files.len());
    assert!(
        files.len() > 100,
        "a full site-day should hold hundreds of objects, got {}",
        files.len()
    );

    let volume = files
        .iter()
        .find(|f| f.name().ends_with("_V06"))
        .expect("at least one V06 volume");
    println!("downloading {}", volume.name());

    let bytes = download_file(&sources, volume.clone())
        .await
        .expect("download should succeed");
    println!("downloaded {} bytes", bytes.len());
    assert!(
        bytes.len() > 1_000_000,
        "a Level II volume should be megabytes, got {}",
        bytes.len()
    );

    let scan = nexrad_data::volume::File::new(bytes)
        .scan()
        .expect("volume should decode");
    let sweeps = scan.sweeps().len();
    println!("decoded {sweeps} sweeps");
    assert!(sweeps > 0, "decoded a volume with no sweeps");
}

#[ignore = "hits the live unidata-nexrad-level2 S3 bucket"]
#[tokio::test]
async fn live_paged_listing_equals_the_single_page_listing() {
    let sources = DataSources::production();
    let client = shared_client();
    let prefix = day_prefix("KTLX", &date(2024, 5, 20));

    let mut single_page_requests = 0;
    let whole = collect_keys(
        &sources.s3_bucket_url(&sources.level2_bucket),
        &prefix,
        None,
        |url| {
            single_page_requests += 1;
            get_text(client, url)
        },
    )
    .await
    .expect("unpaginated listing");

    let mut paged_requests = 0;
    let paged = collect_keys(
        &sources.s3_bucket_url(&sources.level2_bucket),
        &prefix,
        Some(20),
        |url| {
            paged_requests += 1;
            get_text(client, url)
        },
    )
    .await
    .expect("paginated listing");

    println!(
        "{} keys in {single_page_requests} request(s); {} keys in {paged_requests} request(s)",
        whole.len(),
        paged.len()
    );

    assert_eq!(
        single_page_requests, 1,
        "the default page should hold a site-day"
    );
    assert!(
        paged_requests > 5,
        "max-keys=20 over ~235 keys should need many pages, took {paged_requests}"
    );
    assert_eq!(
        paged, whole,
        "the paged listing differs from the single-page listing"
    );
}

#[ignore = "hits the live unidata-nexrad-level2 S3 bucket"]
#[tokio::test]
async fn live_missing_volume_is_reported_as_not_found() {
    // Well-formed name, real site, real date, no such volume: this reaches
    // S3 and comes back 404 rather than failing key derivation locally.
    let missing = Identifier::new("KTLX20240520_010101_V06".to_string());
    let err = download_file(&DataSources::production(), missing)
        .await
        .expect_err("a nonexistent volume must not download");
    println!("got: {err:?}");
    assert!(
        matches!(err, ArchiveError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

/// The claim this whole module rests on: no SigV4, no credential chain.
#[ignore = "hits the live unidata-nexrad-level2 S3 bucket"]
#[tokio::test]
async fn live_listing_needs_no_credentials() {
    let sources = DataSources::production();
    let url = list_url(
        &sources.s3_bucket_url(&sources.level2_bucket),
        "2024/05/20/KTLX",
        Some(1),
        None,
    )
    .expect("url");
    let response = shared_client()
        .get(&url)
        .send()
        .await
        .expect("request should reach S3");
    println!("anonymous LIST -> {}", response.status());
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "anonymous listing was refused; this module would need SigV4"
    );
}

#[ignore = "hits the live unidata-nexrad-level2 S3 bucket"]
#[tokio::test]
async fn live_volume_elevation_numbers_are_contiguous_and_terminated() {
    use nexrad_model::data::RadialStatus;

    let sources = DataSources::production();
    let day = date(2024, 5, 20);
    let files = list_files(&sources, "KTLX", &day).await.expect("listing");
    let volume = files
        .iter()
        .find(|f| f.name().ends_with("_V06"))
        .expect("at least one V06 volume");
    let scan = nexrad_data::volume::File::new(
        download_file(&sources, volume.clone())
            .await
            .expect("download"),
    )
    .scan()
    .expect("decode");

    println!(
        "{} -> VCP {}, {} sweeps",
        volume.name(),
        scan.coverage_pattern_number().number(),
        scan.sweeps().len()
    );

    let mut numbers = Vec::new();
    for sweep in scan.sweeps() {
        let radials = sweep.radials();
        let first = radials.first().expect("sweep with no radials");
        let last = radials.last().expect("sweep with no radials");
        println!(
            "  elev {:>2}  {:>5.2}°  {:>4} radials  spacing {:.1}°  last status {:?}",
            sweep.elevation_number(),
            first.elevation_angle_degrees(),
            radials.len(),
            first.azimuth_spacing_degrees(),
            last.radial_status(),
        );
        numbers.push(sweep.elevation_number());
    }

    let expected: Vec<u8> = (1..=scan.sweeps().len() as u8).collect();
    assert_eq!(
        numbers, expected,
        "elevation numbers are not contiguous 1..=n; a BTreeMap keyed on \
             them would merge distinct cuts"
    );

    let terminators: Vec<RadialStatus> = scan
        .sweeps()
        .iter()
        .map(|s| s.radials().last().expect("radials").radial_status())
        .collect();
    let (last, rest) = terminators.split_last().expect("at least one sweep");
    for (i, status) in rest.iter().enumerate() {
        assert!(
            matches!(status, RadialStatus::ElevationEnd),
            "sweep {} ends on {:?}, not ElevationEnd — the seal rule would \
                 leave it open",
            i + 1,
            status
        );
    }
    assert!(
        matches!(last, RadialStatus::ScanEnd | RadialStatus::ElevationEnd),
        "the final sweep ends on {last:?}; the seal rule must accept it"
    );
    println!("final sweep terminator: {last:?}");
}
