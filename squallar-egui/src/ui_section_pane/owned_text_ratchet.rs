//! **A string this file already owns is never handed to [`egui::Painter::text`].**
//!
//! `Painter::text` takes `impl ToString` and calls `to_string()` on it, and for a
//! `String` that is a clone — so `painter.text(pos, anchor, format!(..), ..)`
//! allocates the label twice and drops one copy unread. Five sites in this file
//! did that on every frame a section pane drew: the height ticks, the distance
//! ticks, the two axis unit labels and the line readout.
//!
//! `paint_owned_text` hands the layout the string it was given. This scan is what
//! stops the shorter spelling coming back; it runs over the source of
//! `ui_section_pane.rs` itself, and the control below is what shows the scan can
//! still see the shape it forbids.

/// The file this ratchet governs, relative to the crate root.
const SWEPT: &str = "src/ui_section_pane.rs";

/// Line numbers of `.text(..)` calls whose argument list builds its own string.
///
/// Crude on purpose: it reads the argument list of every `.text(` call and looks
/// for the three spellings that produce an owned `String` at the call site. A
/// borrowed `&str` argument costs the one allocation `Painter::text` cannot avoid
/// and is not what this forbids.
fn owned_text_arguments(src: &str) -> Vec<usize> {
    let mut hits = Vec::new();
    let mut at = 0;
    while let Some(found) = src[at..].find(".text(") {
        let open = at + found + ".text(".len();
        let mut depth = 1usize;
        let mut end = open;
        for (i, c) in src[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let args = &src[open..end];
        if args.contains("format!") || args.contains(".to_owned()") || args.contains(".to_string()")
        {
            hits.push(src[..open].matches('\n').count() + 1);
        }
        at = open;
    }
    hits
}

/// The scan can see the shape it forbids, and does not see the shape it allows.
///
/// Without this the ratchet below could pass by being blind rather than by the
/// file being clean.
#[test]
fn the_scan_reads_the_argument_list_and_not_the_call() {
    let forbidden = "    painter.text(\n        pos,\n        anchor,\n        \
                     format!(\"{x:.0}\"),\n        font,\n        color,\n    );\n";
    assert_eq!(
        owned_text_arguments(forbidden),
        vec![1],
        "the scan cannot see a `format!` handed to `Painter::text`, so its \
         silence over the real file would mean nothing"
    );

    let allowed = "    painter.text(pos, anchor, \"A\", font, color);\n";
    assert!(
        owned_text_arguments(allowed).is_empty(),
        "the scan flags a borrowed `&str`, which pays the one allocation \
         `Painter::text` cannot avoid and is not the defect"
    );

    let nested = "    painter.text(pos, anchor, label(format!(\"{x}\")), font, color);\n";
    assert_eq!(
        owned_text_arguments(nested),
        vec![1],
        "the scan stops at the first nested parenthesis instead of reading the \
         whole argument list"
    );
}

/// No site in `ui_section_pane.rs` hands `Painter::text` a string it already owns.
#[test]
fn the_section_pane_never_pays_for_its_labels_twice() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SWEPT);
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the swept file must be readable at {}: {e}", path.display()));
    assert!(
        src.len() > 10_000,
        "{} read back {} bytes, so a silent scan below would prove nothing",
        path.display(),
        src.len()
    );
    assert!(
        src.contains("fn paint_owned_text"),
        "the swept file no longer carries the helper this ratchet exists to keep \
         callers on"
    );

    let hits = owned_text_arguments(&src);
    assert!(
        hits.is_empty(),
        "lines {hits:?} of {} hand `Painter::text` a string they built \
         themselves, so each of those labels is allocated twice on every frame a \
         section pane draws. Use `paint_owned_text`.",
        path.display()
    );
}
