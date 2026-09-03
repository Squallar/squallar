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

/// The `SUMMARY transport:` printer in drive.py, as one folded line.
fn summary_printer() -> String {
    let at = DRIVE
        .find(r#"print("[%s] SUMMARY transport: %s replies"#)
        .expect("drive.py no longer prints a transport summary");
    let rest = &DRIVE[at..];
    let end = rest.find("))").expect("the print call is unterminated");
    rest[..end].split_whitespace().collect::<Vec<_>>().join(" ")
}

/// **Every figure the rig PARSES, the rig also PRINTS.**
///
/// This gate exists because its absence already cost a measurement. The two
/// timing fields were added to the regex and to its consumer and not to the
/// summary, so they reached the artifact and never reached stdout — and the
/// author of that change had, hours earlier the same day, fixed the identical
/// defect elsewhere in this file (`watcher_named_in`, where two whole windowed
/// cut families were computed and never printed).
///
/// A figure parsed and not printed is not "less visible". It is invisible to
/// whoever runs the leg, and indistinguishable from a figure the app never
/// emitted.
///
/// # What this deliberately does NOT cover, and why that is not an oversight
///
/// It counts `%s` in the format string, not the arguments supplied to it. A
/// summary that keeps a `%s` and drops its argument is therefore invisible
/// here — and it does not need to be caught here, because **Python raises**:
/// `"%s %s" % ("a",)` is `TypeError: not enough arguments for format string`,
/// and the reverse is `not all arguments converted`. Verified, not assumed.
/// A leg would die loudly on the first summary it printed. The gap this test
/// exists to close is the SILENT one: a field the pattern reads and the
/// sentence never mentions, which no interpreter can see.
#[test]
fn every_transport_figure_the_rig_reads_is_also_printed() {
    let printed = summary_printer();
    let read = transport_re().matches(r"(\d+)").count();
    let written = printed.matches("%s").count() - 1; // the leading `[%s]` tag
    assert_eq!(
        written, read,
        "the rig reads {read} transport figures and prints {written}; a field \
         was added to the pattern and not to the summary, so it reaches the \
         artifact and never reaches stdout: {printed}",
    );
    for label in ["us encoding", "us posting"] {
        assert!(
            printed.contains(label),
            "the transport summary does not print {label:?}: {printed}",
        );
    }
}
