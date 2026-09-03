//! The `transport:` line and the rig regex that reads it are one sentence
//! written in two languages, and nothing else holds them equal.
//!
//! `worker_port` is `cfg(target_arch = "wasm32")`, so no host test can call it
//! and no host test can render the line. What a host test CAN do is read both
//! files as text and hold their field lists equal — the shape
//! `linear_memory_ceiling.rs` already uses for the link-time heap ceiling.
//!
//! Why this matters more than a usual line pin: `native_row.py` reads drive.py's
//! patterns at RUN TIME and `int()`s every group, so a regex that stops matching
//! does not error — the reading goes null, and a null is indistinguishable from
//! "the transport never ran". This campaign has already lost time to exactly
//! that shape twice (a bundle-vs-rig field skew that two lanes read as a
//! renderer regression, and a summary filter that hid two whole cut families).
#![cfg(not(target_arch = "wasm32"))]

const PORT: &str = include_str!("../src/worker_port.rs");
const DRIVE: &str = include_str!("../../.github/browser-rig/drive.py");

/// The `transport_re` line of drive.py.
fn transport_re() -> &'static str {
    DRIVE
        .lines()
        .find(|l| l.starts_with("var transport_re = "))
        .expect("drive.py no longer declares `transport_re`; the rig cannot read this line")
}

/// The Rust format string, with its `\` continuations and indentation folded
/// away so it can be compared field by field.
fn format_string() -> String {
    let at = PORT
        .find("\"transport: {} replies,")
        .expect("worker_port.rs no longer writes a `transport:` line");
    let rest = &PORT[at..];
    let end = rest
        .find("\",\n")
        .expect("the format string is unterminated");
    rest[..end]
        .replace("\\\n", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// **Every field in the sentence is in the regex, in the same order.**
#[test]
fn the_transport_line_and_the_rig_regex_name_the_same_fields_in_order() {
    let fmt = format_string();
    let re = transport_re();
    let labels = [
        "replies",
        "B out with",
        "B copied out of the worker",
        "B in with",
        "B copied out of this page",
        "us encoding",
        "us posting",
    ];
    let (mut f_at, mut r_at) = (0usize, 0usize);
    for label in labels {
        let f = fmt[f_at..]
            .find(label)
            .unwrap_or_else(|| panic!("the app's line no longer says {label:?}: {fmt}"));
        let r = re[r_at..]
            .find(label)
            .unwrap_or_else(|| panic!("the rig regex no longer reads {label:?}: {re}"));
        f_at += f + label.len();
        r_at += r + label.len();
    }
}

/// **The counts agree, so a field added to one and not the other reddens.**
/// `{}` in the format string against `(\d+)` in the regex.
#[test]
fn the_line_writes_exactly_as_many_figures_as_the_rig_reads() {
    let written = format_string().matches("{}").count();
    let read = transport_re().matches(r"(\d+)").count();
    assert_eq!(
        written, read,
        "the app writes {written} figures and the rig reads {read}; a field was \
         added to one side only, and the rig's reading would go null rather \
         than error",
    );
    assert!(
        written >= 7,
        "expected at least the seven known fields, got {written}"
    );
}

/// **No optional groups.** `native_row.py` reads these patterns at run time and
/// `int()`s every group; a non-participating optional hands it `None` and it
/// dies with a type error on the OLD line only — so the arm running the old
/// binary is the one that appears broken.
#[test]
fn the_transport_pattern_has_no_optional_groups() {
    let re = transport_re();
    for forbidden in ["(?:", ")?", "*)", "|"] {
        assert!(
            !re.contains(forbidden),
            "`transport_re` contains {forbidden:?}, which can make a group \
             non-participating: {re}",
        );
    }
}
