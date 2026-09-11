//! The scanner is checked against `xml-rs`, which parsed these responses in the
//! shipped build until the scanner replaced it. The oracle below is that reader
//! driven by the event loop `parse_list_page` used to run, so a corpus entry
//! disagreeing is a difference between the two implementations and not a
//! difference from a transcription of one.

use super::*;

/// The event loop `parse_list_page` ran on `xml-rs`, kept verbatim as the oracle.
fn oracle(body: &str) -> ListPage {
    use xml::reader::{EventReader, XmlEvent};

    let mut page = ListPage::default();
    let mut field: Option<Field> = None;
    let mut in_contents = false;
    let mut in_common_prefixes = false;
    let mut buffer = String::new();

    for event in EventReader::new(body.as_bytes()) {
        let event = event.expect("corpus entry parses as XML");
        match event {
            XmlEvent::StartElement { name, .. } => {
                buffer.clear();
                field = match name.local_name.as_str() {
                    "Contents" => {
                        in_contents = true;
                        None
                    }
                    "CommonPrefixes" => {
                        in_common_prefixes = true;
                        None
                    }
                    "Key" if in_contents => Some(Field::Key),
                    "Prefix" if in_common_prefixes => Some(Field::CommonPrefix),
                    "IsTruncated" => Some(Field::IsTruncated),
                    "NextContinuationToken" => Some(Field::NextToken),
                    _ => None,
                };
            }
            XmlEvent::Characters(chars) => {
                if field.is_some() {
                    buffer.push_str(&chars);
                }
            }
            XmlEvent::EndElement { name } => {
                match name.local_name.as_str() {
                    "Contents" => in_contents = false,
                    "CommonPrefixes" => in_common_prefixes = false,
                    "Key" if field == Some(Field::Key) => {
                        page.keys.push(std::mem::take(&mut buffer));
                    }
                    "Prefix" if field == Some(Field::CommonPrefix) => {
                        page.common_prefixes.push(std::mem::take(&mut buffer));
                    }
                    "IsTruncated" if field == Some(Field::IsTruncated) => {
                        page.truncated = buffer.trim() == "true";
                    }
                    "NextContinuationToken" if field == Some(Field::NextToken) => {
                        page.next_token = Some(std::mem::take(&mut buffer));
                    }
                    _ => {}
                }
                field = None;
                buffer.clear();
            }
            _ => {}
        }
    }
    page
}

const NS: &str = r#" xmlns="http://s3.amazonaws.com/doc/2006-03-01/""#;

/// Each entry is `(what shape it carries, document)`. Every one is a document
/// `xml-rs` accepts, because the oracle has to be able to read it too.
fn corpus() -> Vec<(&'static str, String)> {
    vec![
        (
            "a plain keyed page with a cursor",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult{NS}><Name>b</Name>\
<Prefix>2024/05/20/KTLX</Prefix><IsTruncated>true</IsTruncated>\
<NextContinuationToken>TOK</NextContinuationToken>\
<Contents><Key>a/b/c/d/one</Key><Size>1</Size></Contents>\
<Contents><Key>a/b/c/d/two</Key><Size>2</Size></Contents></ListBucketResult>"#
            ),
        ),
        (
            "an empty listing",
            format!(
                r#"<ListBucketResult{NS}><Prefix>p</Prefix>\
<IsTruncated>false</IsTruncated></ListBucketResult>"#
            ),
        ),
        (
            "a delimited listing, prefixes only",
            format!(
                r#"<ListBucketResult{NS}><Prefix>KTLX/</Prefix><Delimiter>/</Delimiter>\
<IsTruncated>false</IsTruncated><CommonPrefixes><Prefix>KTLX/1/</Prefix></CommonPrefixes>\
<CommonPrefixes><Prefix>KTLX/10/</Prefix></CommonPrefixes></ListBucketResult>"#
            ),
        ),
        (
            "a Key outside Contents, which belongs to nobody",
            format!(
                r#"<ListBucketResult{NS}><IsTruncated>false</IsTruncated>\
<Error><Code>NoSuchKey</Code><Key>a/b/c/d/phantom</Key></Error>\
<Contents><Key>a/b/c/d/real</Key></Contents></ListBucketResult>"#
            ),
        ),
        (
            "entity references inside a key and a token",
            format!(
                r#"<ListBucketResult{NS}><IsTruncated>true</IsTruncated>\
<NextContinuationToken>a&amp;b&#61;c</NextContinuationToken>\
<Contents><Key>a/b&amp;c/d&lt;e&gt;f&quot;g&apos;h</Key>\
<ETag>&quot;deadbeef&quot;</ETag></Contents>\
<Contents><Key>num/&#65;&#x42;</Key></Contents></ListBucketResult>"#
            ),
        ),
        (
            "namespace-prefixed element names",
            r#"<s3:ListBucketResult xmlns:s3="http://s3.amazonaws.com/doc/2006-03-01/">\
<s3:IsTruncated>false</s3:IsTruncated>\
<s3:Contents><s3:Key>a/b/c/d/pfx</s3:Key></s3:Contents></s3:ListBucketResult>"#
                .to_owned(),
        ),
        (
            "a self-closing Key, which stands for an empty value",
            format!(
                r#"<ListBucketResult{NS}><IsTruncated>false</IsTruncated>\
<Contents><Key/><Size>0</Size></Contents>\
<Contents><Key>a/b/c/d/after</Key></Contents></ListBucketResult>"#
            ),
        ),
        (
            "a CDATA section carrying a key",
            format!(
                r#"<ListBucketResult{NS}><IsTruncated>false</IsTruncated>\
<Contents><Key><![CDATA[a/b/c/d/cdata&raw]]></Key></Contents></ListBucketResult>"#
            ),
        ),
        (
            "a comment splitting a key's character data",
            format!(
                r#"<ListBucketResult{NS}><IsTruncated>false</IsTruncated>\
<Contents><Key>a/b/c/<!-- half way -->d/split</Key></Contents></ListBucketResult>"#
            ),
        ),
        (
            "an attribute value carrying a > that is not a tag end",
            format!(
                r#"<ListBucketResult{NS}><IsTruncated>false</IsTruncated>\
<Contents note="a &gt; b" other='c>d'><Key>a/b/c/d/attr</Key></Contents></ListBucketResult>"#
            ),
        ),
        (
            "whitespace between elements, as a pretty-printed response",
            format!(
                "<ListBucketResult{NS}>\n  <IsTruncated>false</IsTruncated>\n  \
                 <Contents>\n    <Key>a/b/c/d/pretty</Key>\n  </Contents>\n</ListBucketResult>"
            ),
        ),
        (
            "IsTruncated padded with whitespace",
            format!(
                r#"<ListBucketResult{NS}><IsTruncated>  true
</IsTruncated><NextContinuationToken>T</NextContinuationToken></ListBucketResult>"#
            ),
        ),
        (
            "the document's own echoed Prefix, which is not a directory",
            format!(
                r#"<ListBucketResult{NS}><Prefix>2024/05/20/KTLX/</Prefix>\
<IsTruncated>false</IsTruncated></ListBucketResult>"#
            ),
        ),
    ]
}

/// Continuation lines in the raw literals above are written `\` + newline for
/// readability; the documents must not actually carry them.
fn document(raw: &str) -> String {
    raw.replace("\\\n", "")
}

/// The one corpus entry the scanner deliberately does not reproduce, pinned by
/// `a_cdata_key_is_read_where_the_old_reader_dropped_it`.
const REPAIRED: &str = "a CDATA section carrying a key";

#[test]
fn scanner_matches_the_xml_reader_it_replaced_across_the_corpus() {
    let mut checked = 0;
    for (shape, raw) in corpus() {
        if shape == REPAIRED {
            continue;
        }
        let doc = document(&raw);
        let expected = oracle(&doc);
        let got = parse_list_page(&doc).unwrap_or_else(|e| panic!("{shape}: scanner failed: {e}"));
        assert_eq!(got, expected, "{shape}: scanner and xml-rs disagree\n{doc}");
        checked += 1;
    }
    assert_eq!(
        checked,
        corpus().len() - 1,
        "exactly one corpus entry is a known divergence; the rest must be compared"
    );
}

/// The old event loop matched `Characters` and not `CData`, which
/// `EventReader::new` reports as its own event, so a key delivered in a CDATA
/// section was silently read as the empty string. S3 escapes with entity
/// references rather than CDATA, so nothing ever hit it -- but an empty key is
/// the worst available failure, because it reaches `key_to_identifier` as a
/// well-formed nothing rather than an error.
///
/// The scanner reads it. This pins both halves, so the divergence stays a
/// deliberate repair and not a difference nobody noticed.
#[test]
fn a_cdata_key_is_read_where_the_old_reader_dropped_it() {
    let (shape, raw) = corpus()
        .into_iter()
        .find(|(shape, _)| *shape == REPAIRED)
        .expect("the corpus carries the CDATA entry");
    let doc = document(&raw);
    assert!(doc.contains("<![CDATA["), "{shape} lost its CDATA section");

    assert_eq!(
        oracle(&doc).keys,
        vec![""],
        "the old reader is supposed to drop a CDATA key to the empty string; if it no longer \
         does, this repair is testing nothing"
    );
    assert_eq!(
        parse_list_page(&doc).expect("parses").keys,
        vec!["a/b/c/d/cdata&raw"],
        "the scanner must read a CDATA key, ampersand and all, and not decode it as an entity"
    );
}

/// The corpus is only worth what it reaches. A corpus of documents that all
/// look alike would pass while the shapes it names go untested.
#[test]
fn the_corpus_reaches_every_shape_it_claims() {
    let docs: Vec<String> = corpus().iter().map(|(_, raw)| document(raw)).collect();
    let all = docs.join("");
    for needle in [
        "&amp;",
        "&#65;",
        "&#x42;",
        "s3:Key",
        "<Key/>",
        "<![CDATA[",
        "<!--",
        "c>d",
        "\n  ",
    ] {
        assert!(
            all.contains(needle),
            "no corpus entry carries {needle:?}, so nothing tests that shape"
        );
    }
    assert!(
        docs.iter().any(|d| !d.contains("<Contents>")),
        "no corpus entry is keyless, so the empty and delimited paths go untested"
    );
    // A corpus entry that parses to nothing asserts nothing about the keys.
    let keyed = docs
        .iter()
        .filter(|d| !parse_list_page(d).expect("parses").keys.is_empty())
        .count();
    assert!(
        keyed >= 7,
        "only {keyed} corpus entries yield any key at all"
    );
}

#[test]
fn a_truncated_document_is_a_malformed_listing_and_not_a_silent_short_page() {
    let doc = document(&corpus()[0].1);
    let cut = &doc[..doc.len() - 30];
    let err = parse_list_page(cut).expect_err("a document cut mid-element must not parse clean");
    assert!(
        matches!(err, ArchiveError::MalformedListing(_)),
        "wrong error for a cut document: {err:?}"
    );

    let mid_tag = format!("{}<Contents", doc);
    let err = parse_list_page(&mid_tag).expect_err("an unterminated tag must not parse clean");
    assert!(
        matches!(err, ArchiveError::MalformedListing(_)),
        "wrong error for an unterminated tag: {err:?}"
    );
}

#[test]
fn an_unknown_entity_reference_is_reported_rather_than_pasted_through() {
    let doc = document(&format!(
        r#"<ListBucketResult{NS}><IsTruncated>false</IsTruncated>\
<Contents><Key>a/b/c/d/&nope;</Key></Contents></ListBucketResult>"#
    ));
    let err = parse_list_page(&doc).expect_err("an unknown entity must not be pasted through");
    assert!(
        matches!(&err, ArchiveError::MalformedListing(m) if m.contains("&nope;")),
        "the error should name the entity it could not read: {err:?}"
    );
}

/// The scar this counter exists for: a cut whose precondition never held on the
/// arm that ran it, reading zero for every tick while nobody looked. This drives
/// the real entry point, not `parse_list_page` directly, so a listing path that
/// stopped reaching the scanner would fail here.
#[test]
fn listing_counter_moves_through_the_real_listing_entry_point() {
    let page = document(
        r#"<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">\
<IsTruncated>false</IsTruncated><Contents><Key>a/b/c/d/counted</Key></Contents>\
</ListBucketResult>"#,
    );
    let wire = page.len() as u64;

    let (pages_before, bytes_before) = scanned_totals();

    let sources = crate::sources::DataSources::production();
    let bucket_url = sources.s3_bucket_url(&sources.level2_bucket);
    let fut = crate::archive::collect_keys(&bucket_url, "2024/05/20/KTLX", None, |_url| {
        let body = page.clone();
        async move { Ok(body) }
    });
    let mut fut = Box::pin(fut);
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    let keys = match fut.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(v) => v.expect("listing completes"),
        std::task::Poll::Pending => panic!("fixture fetcher never yields"),
    };
    assert_eq!(keys, vec!["a/b/c/d/counted"]);

    let (pages_after, bytes_after) = scanned_totals();
    // Deltas, not absolutes: these counters are process-global and other tests
    // in this binary scan pages of their own.
    assert!(
        pages_after > pages_before,
        "collect_keys parsed a page but the page counter did not move: {pages_before} -> \
         {pages_after}"
    );
    assert!(
        bytes_after >= bytes_before + wire,
        "collect_keys scanned {wire} wire bytes but the byte counter moved only {} B: the \
         listing path is not reaching the scanner",
        bytes_after - bytes_before
    );
}
