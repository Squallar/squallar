//! The one inventory of every icon glyph the chrome may draw, and the tests
//! that keep it honest against the fonts egui actually bundles.
//!
//! # Why this exists
//!
//! egui 0.35's default fonts are Ubuntu-Light plus a *subset* NotoEmoji and
//! the emoji-icon-font, and the subset is far smaller than the names suggest:
//! `▲`, `✕`, `⚡`, `🔗`, `🕐` and dozens of other plausible icon chars have no
//! glyph in the proportional family and render as a tofu box. The first
//! hands-on session found them one button at a time. This module turns that
//! into a closed class:
//!
//! * [`ICON_GLYPHS`] lists every icon char the chrome is allowed to use, and
//!   a test holds each entry against the real font tables
//!   (`epaint::text::Fonts::has_glyph` over `FontDefinitions::default()`) —
//!   an entry the fonts do not carry fails the build, so tofu cannot ship.
//! * A second test scans the crate's UI sources (and the sibling crates'
//!   display strings — the overlay handlers' and the frontend's) and
//!   requires every non-ASCII char in every string literal to be on the
//!   allowlist — [`ICON_GLYPHS`] plus [`TEXT_GLYPHS`] — so a new icon or a
//!   fancy typographic char cannot be introduced without registering it
//!   here, where the font check will judge it.
//!
//! # The prose rules (user decision, M8)
//!
//! UI text is ASCII prose: no em/en dashes (`-` instead), no arrows (write
//! "to"), no middle dots (` - ` is the one separator), no `…` (write `...`),
//! no curly quotes. The degree sign and the other unit chars in
//! [`TEXT_GLYPHS`] are the deliberate exceptions — they are units, not
//! typography, and the font check verifies each. egui's own truncation
//! (`TextWrapMode::Truncate`) appends its ellipsis internally from a glyph
//! its fonts carry; the ban is on *our* string literals, which is what the
//! scan reads.

/// Every icon char the chrome may use, with where it is used — the single
/// source of truth the coverage test walks. All of these are verified against
/// the **proportional** family, which is what every text style in the stock
/// theme resolves to.
///
/// An entry here is a *permission*, checked against the fonts; the source
/// scan is what makes the permission the only route on screen.
pub(crate) const ICON_GLYPHS: &[(char, &str)] = &[
    ('\u{2630}', "app menu (top bar, bottom bar)"),
    ('\u{2699}', "inspector / App (top bar, bottom bar)"),
    ('\u{25a3}', "Layers (top bar, bottom bar)"),
    ('\u{229e}', "Pane (bottom bar)"),
    ('\u{26f6}', "3D region arm (top bar) and the 3D view header"),
    (
        '\u{2215}',
        "cross-section arm (top bar) and the section header",
    ),
    ('\u{23f4}', "back one step; collapse toward the left edge"),
    ('\u{23f5}', "forward / play; restore from the left edge"),
    ('\u{23f6}', "reorder up (stack rows)"),
    (
        '\u{23f7}',
        "reorder down (stack rows); collapse downward (timeline)",
    ),
    (
        '\u{23ee}',
        "previous loop frame; archive posture (phone scan chip)",
    ),
    ('\u{23ed}', "next loop frame"),
    ('\u{23f8}', "pause (transport); auto-poll off"),
    (
        '\u{23fa}',
        "Live (timeline, bottom bar, poll chip, live posture)",
    ),
    ('\u{23f1}', "collapsed time chip"),
    ('\u{221e}', "radar loop toggle"),
    (
        '\u{27f3}',
        "the map pane's stale-image notice - reserved for it",
    ),
    (
        '\u{21bb}',
        "refresh (status bar, layer bodies); rotate the line clockwise",
    ),
    ('\u{1f441}', "layer visibility eye (stack rows)"),
    ('\u{26d3}', "linked to shared time (pills, sync checkboxes)"),
    ('\u{2297}', "unlinked from shared time (pills)"),
    (
        '\u{d7}',
        "close / deselect / dismiss (icon); times in grid sizes and \
         vertical exaggeration (text)",
    ),
    ('\u{2039}', "collapse the stack leftward"),
    (
        '\u{203a}',
        "collapse the inspector rightward; crumb and row chevrons",
    ),
    ('\u{2316}', "pointer readout position marker"),
    ('\u{21ba}', "rotate the section line counter-clockwise"),
];

/// Icon chars carried only by the **monospace** family (Hack), verified
/// against it. Usable only where the draw site names the monospace font
/// explicitly — today that is the section pane's painted ℹ detail toggle.
pub(crate) const MONO_ICON_GLYPHS: &[(char, &str)] = &[(
    '\u{2139}',
    "section-pane detail toggle (painted, monospace)",
)];

/// Non-icon, non-ASCII chars UI text may carry: units and symbols with no
/// ASCII spelling worth the loss. Verified against the proportional family
/// like the icons. A char with both roles is registered once: `×` lives in
/// [`ICON_GLYPHS`], where its text use is documented beside its icon one.
pub(crate) const TEXT_GLYPHS: &[(char, &str)] = &[
    ('\u{b0}', "degrees"),
    ('\u{b2}', "squared (km²)"),
    ('\u{b1}', "plus-minus (iso thresholds)"),
    ('\u{2265}', "at least (iso thresholds)"),
    ('\u{2264}', "at most (iso thresholds)"),
    ('\u{b5}', "micro (µs offsets)"),
    ('\u{a9}', "copyright (basemap attribution)"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Fonts` over egui's stock `FontDefinitions` — exactly what the app
    /// renders with, since nothing in rustdar installs custom fonts.
    fn stock_fonts() -> egui::text::Fonts {
        egui::text::Fonts::new(Default::default(), egui::FontDefinitions::default())
    }

    /// Every registered glyph exists in the family it is registered for. This
    /// is the test that makes the inventory mean something: an icon char the
    /// bundled fonts do not carry fails here by name, instead of shipping as
    /// a tofu box for the user to find.
    #[test]
    fn every_registered_glyph_exists_in_eguis_bundled_fonts() {
        let mut fonts = stock_fonts();
        let prop = egui::FontId::proportional(14.0);
        let mono = egui::FontId::monospace(14.0);
        for &(c, what) in ICON_GLYPHS.iter().chain(TEXT_GLYPHS) {
            assert!(
                fonts.has_glyph(&prop, c),
                "U+{:04X} ({what}) has no glyph in the proportional family: \
                 it would render as a tofu box",
                c as u32,
            );
        }
        for &(c, what) in MONO_ICON_GLYPHS {
            assert!(
                fonts.has_glyph(&mono, c),
                "U+{:04X} ({what}) has no glyph in the monospace family",
                c as u32,
            );
        }
    }

    /// The inventory is a set — across all three tables: a duplicated entry
    /// would make one row dead weight and a removal look safe when it is
    /// not. A char with both an icon and a text role (`×`) gets one entry
    /// documenting both, not one per table.
    #[test]
    fn the_glyph_inventory_has_no_duplicate_entries() {
        let mut seen = std::collections::BTreeSet::new();
        for &(c, _) in ICON_GLYPHS
            .iter()
            .chain(MONO_ICON_GLYPHS)
            .chain(TEXT_GLYPHS)
        {
            assert!(seen.insert(c), "U+{:04X} is registered twice", c as u32);
        }
    }

    // --- the source scan -------------------------------------------------

    /// String-literal contents of one Rust source, comments stripped and
    /// `\u{..}` escapes decoded, with the trailing `#[cfg(test)] mod` block
    /// (assertion prose, not UI text) cut off first.
    fn string_literals(src: &str) -> Vec<String> {
        let src = match find_test_mod(src) {
            Some(at) => &src[..at],
            None => src,
        };
        let bytes: Vec<char> = src.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                '/' if bytes.get(i + 1) == Some(&'/') => {
                    while i < bytes.len() && bytes[i] != '\n' {
                        i += 1;
                    }
                }
                '/' if bytes.get(i + 1) == Some(&'*') => {
                    let mut depth = 1;
                    i += 2;
                    while i < bytes.len() && depth > 0 {
                        if bytes[i] == '/' && bytes.get(i + 1) == Some(&'*') {
                            depth += 1;
                            i += 2;
                        } else if bytes[i] == '*' && bytes.get(i + 1) == Some(&'/') {
                            depth -= 1;
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                }
                'r' if matches!(bytes.get(i + 1), Some('"' | '#')) => {
                    // Raw string: r"..." or r#"..."# etc.
                    let mut hashes = 0;
                    let mut j = i + 1;
                    while bytes.get(j) == Some(&'#') {
                        hashes += 1;
                        j += 1;
                    }
                    if bytes.get(j) != Some(&'"') {
                        i += 1;
                        continue;
                    }
                    j += 1;
                    let start = j;
                    'raw: while j < bytes.len() {
                        if bytes[j] == '"'
                            && bytes[j + 1..]
                                .iter()
                                .take(hashes)
                                .filter(|&&c| c == '#')
                                .count()
                                == hashes
                        {
                            out.push(bytes[start..j].iter().collect());
                            i = j + 1 + hashes;
                            break 'raw;
                        }
                        j += 1;
                    }
                    if j >= bytes.len() {
                        i = j;
                    }
                }
                '\'' => {
                    // A char literal ('x', '\n', '\u{..}') or a lifetime.
                    // Char literals are scanned like strings — a `'…'` pushed
                    // into a `String` is display text the string pass never
                    // sees — and a '"' inside one must not open a string.
                    if bytes.get(i + 1) == Some(&'\\') {
                        if bytes.get(i + 2) == Some(&'u') && bytes.get(i + 3) == Some(&'{') {
                            let mut cp = 0u32;
                            let mut j = i + 4;
                            while j < bytes.len() && bytes[j] != '}' {
                                cp = cp * 16 + bytes[j].to_digit(16).expect("hex escape");
                                j += 1;
                            }
                            if let Some(c) = char::from_u32(cp) {
                                out.push(c.to_string());
                            }
                        }
                        // Skip to the close whatever the escape was; the
                        // non-`\u` escapes all resolve to ASCII.
                        let mut j = i + 2;
                        while j < bytes.len() && bytes[j] != '\'' {
                            j += 1;
                        }
                        i = j + 1;
                    } else if bytes.get(i + 2) == Some(&'\'') {
                        out.push(bytes[i + 1].to_string());
                        i += 3;
                    } else {
                        i += 1; // a lifetime
                    }
                }
                '"' => {
                    let mut cur = String::new();
                    i += 1;
                    while i < bytes.len() && bytes[i] != '"' {
                        if bytes[i] == '\\' {
                            if bytes.get(i + 1) == Some(&'u') && bytes.get(i + 2) == Some(&'{') {
                                let mut j = i + 3;
                                let mut cp = 0u32;
                                while j < bytes.len() && bytes[j] != '}' {
                                    cp = cp * 16 + bytes[j].to_digit(16).expect("hex escape");
                                    j += 1;
                                }
                                if let Some(c) = char::from_u32(cp) {
                                    cur.push(c);
                                }
                                i = j + 1;
                            } else {
                                // Every other escape resolves to ASCII (or a
                                // line continuation); the scan only cares
                                // about non-ASCII, so the spelling is enough.
                                i += 2;
                            }
                        } else {
                            cur.push(bytes[i]);
                            i += 1;
                        }
                    }
                    i += 1;
                    out.push(cur);
                }
                _ => i += 1,
            }
        }
        out
    }

    /// Where the file's `#[cfg(test)] mod { .. }` tail begins, if it has one.
    /// The UI sources keep their inline test modules last, so cutting there
    /// drops assertion messages — which are prose for developers, not UI
    /// text — from the scan. Only a mod with an inline `{` body cuts: a
    /// bodiless `#[cfg(test)] mod tests;` merely names a sibling file, which
    /// the walk already skips by name, and cutting at the declaration would
    /// unscan everything after it — most of `glm/mod.rs`, once.
    fn find_test_mod(src: &str) -> Option<usize> {
        let mut from = 0;
        while let Some(rel) = src[from..].find("#[cfg(test)]") {
            let at = from + rel;
            let tail = src[at + "#[cfg(test)]".len()..].trim_start();
            if let Some(decl) = tail.strip_prefix("mod ") {
                let after_name = decl.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_');
                if after_name.trim_start().starts_with('{') {
                    return Some(at);
                }
            }
            from = at + 1;
        }
        None
    }

    /// The UI sources under scan: this crate's `src/` and the sibling crates
    /// whose strings reach the screen — the overlay crate's display strings
    /// (status lines, control labels, popup text and the fetch errors the
    /// toast presents) and the frontend crate's, whose `VolumePaint::Empty`
    /// prose is the 3D pane's on-screen empty state. Test files carry
    /// developer prose and are skipped by name.
    fn scanned_sources() -> Vec<std::path::PathBuf> {
        let mut roots = vec![
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../rustdar-overlays/src"),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../rustdar-frontend/src"),
        ];
        let mut files = Vec::new();
        while let Some(dir) = roots.pop() {
            for entry in std::fs::read_dir(&dir).expect("source dir must be readable") {
                let path = entry.expect("dir entry").path();
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if path.is_dir() {
                    roots.push(path);
                } else if name.ends_with(".rs")
                    && !name.contains("test")
                    && !matches!(name, "input_harness.rs" | "parity_walk.rs")
                {
                    files.push(path);
                }
            }
        }
        assert!(
            files.len() > 40,
            "the scan found only {} sources - the walk is broken, not the tree",
            files.len()
        );
        files
    }

    /// Every non-ASCII char in every UI string literal is a registered glyph.
    ///
    /// This is the closed class: the banned typography (em/en dashes, arrows,
    /// middle dots, `…`, curly quotes) fails because it is not registered,
    /// and a *new* icon fails until it is added to [`ICON_GLYPHS`] — where
    /// the font-table test decides whether it may exist at all.
    #[test]
    fn ui_string_literals_use_only_registered_glyphs() {
        let allowed: std::collections::BTreeSet<char> = ICON_GLYPHS
            .iter()
            .chain(TEXT_GLYPHS)
            .chain(MONO_ICON_GLYPHS)
            .map(|&(c, _)| c)
            .collect();
        let mut offences = Vec::new();
        for path in scanned_sources() {
            let src = std::fs::read_to_string(&path).expect("source must be readable");
            for literal in string_literals(&src) {
                for c in literal.chars() {
                    if !c.is_ascii() && !allowed.contains(&c) {
                        offences.push(format!(
                            "{}: U+{:04X} {c:?} in {literal:?}",
                            path.display(),
                            c as u32
                        ));
                    }
                }
            }
        }
        assert!(
            offences.is_empty(),
            "unregistered non-ASCII chars in UI strings - either replace them \
             (ASCII prose: '-' for dashes, ' - ' for middots, '...' for \
             ellipses) or register a carried glyph in ui_glyphs.rs:\n{}",
            offences.join("\n")
        );
    }

    /// The scanner itself: literals are found — char literals included, in
    /// both spellings — comments and `\u{..}` escapes are handled, a bodiless
    /// test-mod *declaration* does not end the scan, and the inline
    /// test-module tail is cut. A broken scanner passes the scan vacuously,
    /// so it gets its own pin.
    #[test]
    fn the_literal_scanner_reads_what_rust_would() {
        let src = r##"
// a — in a comment is fine
/* and — in /* nested */ blocks */
const A: &str = "em—dash";
const B: &str = "esc\u{2014}aped";
const C: char = '"';
const D: &str = "after the char literal";
const E: char = '…';
const F: char = '\u{2026}';
#[cfg(test)]
mod declared;
const G: &str = "after the bodiless declaration";
#[cfg(test)]
mod tests {
    const H: &str = "test—prose is not scanned";
}
"##;
        let found = string_literals(src);
        assert_eq!(
            found,
            vec![
                "em\u{2014}dash",
                "esc\u{2014}aped",
                "\"",
                "after the char literal",
                "\u{2026}",
                "\u{2026}",
                "after the bodiless declaration",
            ]
        );
    }
}
