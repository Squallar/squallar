//! `ListObjectsV2` response parsing.
//!
//! Only four things in a `ListBucketResult` are wanted: `Contents/Key`,
//! `CommonPrefixes/Prefix`, `IsTruncated` and `NextContinuationToken`. A
//! general event-driven XML reader prices every *other* element at the same
//! rate: measured on a 1000-key page (227,324 B on the wire), `xml-rs` allocated
//! 10,498,710 B across 107,155 allocations -- 46x the document -- for names,
//! attribute vectors, per-element namespace maps and the character data of the
//! five fields per `Contents` entry that are read by nobody. The scanner below
//! allocates only what it keeps: 54,584 B in 1,010 allocations on that page,
//! and parses it in 330.7 us against 6,592.9 us.
//!
//! It is a scanner and not a parser: it does not validate the document, and a
//! `ListBucketResult` is not arbitrary XML. What it does reproduce exactly is
//! the event stream the reader it replaced produced, for the four fields --
//! namespace prefixes stripped from tag names, entity references decoded in
//! captured values, CDATA taken literally, a self-closing tag standing for an
//! empty value. `listing::tests` pins that against `xml-rs` itself as an oracle
//! over a corpus carrying each of those shapes.

use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{ArchiveError, Result};

/// One page of a `ListObjectsV2` response.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ListPage {
    /// In the order S3 returned them, which is UTF-8 binary order.
    pub(crate) keys: Vec<String>,
    /// The `CommonPrefixes` a delimited listing collapsed everything below into.
    pub(crate) common_prefixes: Vec<String>,
    pub(crate) truncated: bool,
    pub(crate) next_token: Option<String>,
}

/// Which element's character data is currently being accumulated.
#[derive(PartialEq, Eq, Debug)]
enum Field {
    Key,
    CommonPrefix,
    IsTruncated,
    NextToken,
}

/// Pages scanned and wire bytes scanned since the process started.
///
/// Read at `debug` by [`super::collect_keys`] and
/// [`super::collect_common_prefixes`] when a listing completes, and asserted by
/// `listing_counter_moves_through_the_real_listing_entry_point`. A reading of
/// zero pages means no listing ever reached this module, and nothing this file
/// claims applies to the running process.
static PAGES_SCANNED: AtomicU64 = AtomicU64::new(0);
static BYTES_SCANNED: AtomicU64 = AtomicU64::new(0);

/// `(pages, wire bytes)` scanned so far.
pub(crate) fn scanned_totals() -> (u64, u64) {
    (
        PAGES_SCANNED.load(Ordering::Relaxed),
        BYTES_SCANNED.load(Ordering::Relaxed),
    )
}

/// Decode the entity references S3 can emit inside a captured value. A value
/// with no `&` in it is handed back borrowed, which is every realistic key.
fn decode_entities(raw: &str) -> Result<Cow<'_, str>> {
    if !raw.contains('&') {
        return Ok(Cow::Borrowed(raw));
    }
    let malformed =
        |what: String| ArchiveError::MalformedListing(format!("{what} in listing value {raw:?}"));
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let end = tail
            .find(';')
            .ok_or_else(|| malformed("unterminated entity reference".to_owned()))?;
        let name = &tail[1..end];
        out.push(match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let digits = name
                    .strip_prefix('#')
                    .ok_or_else(|| malformed(format!("unknown entity reference &{name};")))?;
                let code = match digits.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16),
                    None => digits.parse::<u32>(),
                }
                .map_err(|_| malformed(format!("malformed character reference &{name};")))?;
                char::from_u32(code).ok_or_else(|| {
                    malformed(format!("character reference &{name}; is not a character"))
                })?
            }
        });
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(Cow::Owned(out))
}

/// A tag's local name: what `xml-rs` reports in `OwnedName::local_name`, which
/// is the part after any namespace prefix.
fn local_name(inner: &str) -> &str {
    let name = inner
        .split(|c: char| c.is_ascii_whitespace() || c == '/')
        .next()
        .unwrap_or("");
    match name.split_once(':') {
        Some((_prefix, local)) => local,
        None => name,
    }
}

/// The `>` closing the tag opened at `open`, skipping any inside a quoted
/// attribute value, where XML permits it unescaped.
fn tag_end(body: &str, open: usize) -> Result<usize> {
    let bytes = body.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate().skip(open + 1) {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Ok(i),
            None => {}
        }
    }
    Err(ArchiveError::MalformedListing(format!(
        "unterminated tag at byte {open} of a {} B listing",
        body.len()
    )))
}

/// What a `<` introduces.
enum Tag<'a> {
    Start {
        name: &'a str,
        self_closing: bool,
    },
    End {
        name: &'a str,
    },
    /// A declaration, comment or CDATA section, carrying its literal text if it
    /// contributes any.
    Other {
        text: Option<&'a str>,
    },
}

/// Scan the tag opening at `open`, with the offset just past it.
fn scan_tag(body: &str, open: usize) -> Result<(Tag<'_>, usize)> {
    let after = &body[open + 1..];
    if let Some(rest) = after.strip_prefix("!--") {
        let end = rest.find("-->").ok_or_else(|| {
            ArchiveError::MalformedListing(format!("unterminated comment at byte {open}"))
        })?;
        return Ok((Tag::Other { text: None }, open + 4 + end + 3));
    }
    if let Some(rest) = after.strip_prefix("![CDATA[") {
        let end = rest.find("]]>").ok_or_else(|| {
            ArchiveError::MalformedListing(format!("unterminated CDATA section at byte {open}"))
        })?;
        return Ok((
            Tag::Other {
                text: Some(&rest[..end]),
            },
            open + 9 + end + 3,
        ));
    }
    let close = tag_end(body, open)?;
    let inner = &body[open + 1..close];
    if let Some(name) = inner.strip_prefix('/') {
        return Ok((
            Tag::End {
                name: local_name(name),
            },
            close + 1,
        ));
    }
    if inner.starts_with('?') || inner.starts_with('!') {
        return Ok((Tag::Other { text: None }, close + 1));
    }
    Ok((
        Tag::Start {
            name: local_name(inner),
            self_closing: inner.ends_with('/'),
        },
        close + 1,
    ))
}

/// The character data of the element being captured, already decoded. The one
/// contiguous span that always occurs is borrowed; only a value split by a
/// comment or a CDATA section is assembled.
///
/// Decoding happens per segment and not on the whole value, because the two
/// kinds of segment decode differently: entity references in ordinary character
/// data are resolved, and the content of a CDATA section is literal. A key
/// reading `<![CDATA[a&raw]]>` carries a real ampersand.
#[derive(Default)]
struct Captured<'a>(Option<Cow<'a, str>>);

impl<'a> Captured<'a> {
    /// Ordinary character data: entity references resolved.
    fn push_text(&mut self, span: &'a str) -> Result<()> {
        match decode_entities(span)? {
            Cow::Borrowed(plain) => self.append(Cow::Borrowed(plain)),
            Cow::Owned(decoded) => self.append(Cow::Owned(decoded)),
        }
        Ok(())
    }

    /// A CDATA section's content, taken exactly as written.
    fn push_literal(&mut self, span: &'a str) {
        self.append(Cow::Borrowed(span));
    }

    fn append(&mut self, span: Cow<'a, str>) {
        match self.0.take() {
            None => self.0 = Some(span),
            Some(first) => {
                let mut owned = first.into_owned();
                owned.push_str(&span);
                self.0 = Some(Cow::Owned(owned));
            }
        }
    }

    fn take(&mut self) -> Cow<'a, str> {
        self.0.take().unwrap_or(Cow::Borrowed(""))
    }
}

/// Parse one `ListBucketResult` document.
pub(crate) fn parse_list_page(body: &str) -> Result<ListPage> {
    PAGES_SCANNED.fetch_add(1, Ordering::Relaxed);
    BYTES_SCANNED.fetch_add(body.len() as u64, Ordering::Relaxed);

    let mut page = ListPage::default();
    let mut field: Option<Field> = None;
    let mut in_contents = false;
    let mut in_common_prefixes = false;
    let mut captured = Captured::default();
    let mut cursor = 0usize;
    let mut depth = 0i32;

    while let Some(rel) = body[cursor..].find('<') {
        let open = cursor + rel;
        if field.is_some() && rel > 0 {
            captured.push_text(&body[cursor..open])?;
        }
        let (tag, next) = scan_tag(body, open)?;
        cursor = next;
        match tag {
            Tag::Other { text } => {
                if let (Some(text), true) = (text, field.is_some()) {
                    captured.push_literal(text);
                }
            }
            Tag::Start {
                name,
                self_closing: false,
            } => {
                captured.0 = None;
                depth += 1;
                field = classify(name, in_contents, in_common_prefixes);
                match name {
                    "Contents" => in_contents = true,
                    "CommonPrefixes" => in_common_prefixes = true,
                    _ => {}
                }
            }
            Tag::Start {
                name,
                self_closing: true,
            } => {
                // The reader this replaced reported a start and an end with
                // nothing between, so an empty value is captured and kept.
                captured.0 = None;
                if let Some(empty) = classify(name, in_contents, in_common_prefixes) {
                    close_field(&mut page, &empty, &mut captured);
                }
                field = None;
            }
            Tag::End { name } => {
                depth -= 1;
                if depth < 0 {
                    return Err(ArchiveError::MalformedListing(format!(
                        "end tag </{name}> closes an element that was never opened"
                    )));
                }
                match name {
                    "Contents" => in_contents = false,
                    "CommonPrefixes" => in_common_prefixes = false,
                    _ => {}
                }
                if let Some(open_field) = field.take().filter(|f| closes(name, f)) {
                    close_field(&mut page, &open_field, &mut captured);
                }
                field = None;
                captured.0 = None;
            }
        }
    }

    if depth != 0 {
        // The old reader reported a document that stopped mid-element as an
        // error. Returning the keys scanned so far would make a listing cut in
        // transit look like a complete short page.
        return Err(ArchiveError::MalformedListing(format!(
            "listing ended with {depth} element(s) still open after {} B; it is truncated, and \
             the {} keys scanned so far are not a complete page",
            body.len(),
            page.keys.len()
        )));
    }

    Ok(page)
}

/// Which field, if any, an element's character data belongs to.
fn classify(name: &str, in_contents: bool, in_common_prefixes: bool) -> Option<Field> {
    match name {
        "Key" if in_contents => Some(Field::Key),
        "Prefix" if in_common_prefixes => Some(Field::CommonPrefix),
        "IsTruncated" => Some(Field::IsTruncated),
        "NextContinuationToken" => Some(Field::NextToken),
        _ => None,
    }
}

/// Whether this end tag closes the element currently being captured.
fn closes(name: &str, field: &Field) -> bool {
    matches!(
        (name, field),
        ("Key", Field::Key)
            | ("Prefix", Field::CommonPrefix)
            | ("IsTruncated", Field::IsTruncated)
            | ("NextContinuationToken", Field::NextToken)
    )
}

fn close_field(page: &mut ListPage, field: &Field, captured: &mut Captured<'_>) {
    match field {
        Field::Key => page.keys.push(captured.take().into_owned()),
        Field::CommonPrefix => page.common_prefixes.push(captured.take().into_owned()),
        Field::IsTruncated => page.truncated = captured.take().trim() == "true",
        Field::NextToken => page.next_token = Some(captured.take().into_owned()),
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
