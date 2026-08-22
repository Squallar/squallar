/// The App-pokes-Gui coupling (a field access of `gui` on `self`, dot included), split so
/// this file cannot count itself.
const SELF_GUI: &str = concat!("self.", "gui.");
const SELF_GUI_SET: &str = concat!("self.", "gui.", "set_");
/// The seam call through that coupling; its presence is the control that the scrape is
/// reading real, current source.
const SELF_GUI_APPLY: &str = concat!("self.", "gui.", "apply");

const APP: &str = include_str!("../app.rs");
const APP_FETCH: &str = include_str!("../app_fetch.rs");
const APP_RENDER: &str = include_str!("../app_render.rs");
const APP_CHUNKS: &str = include_str!("../app_chunks.rs");

/// The file with every run of whitespace removed, so a call wrapped across lines counts
/// exactly like one that is not.
fn collapsed(source: &str) -> String {
    source.split_whitespace().collect()
}

#[test]
fn no_production_file_pushes_through_a_gui_setter() {
    // Presence control: the seam really is in the scraped source.
    assert!(
        collapsed(APP).contains(SELF_GUI_APPLY),
        "control: app.rs no longer contains `{SELF_GUI_APPLY}` — the scrape \
         is not reading the seam it exists to guard",
    );
    for (name, source) in [
        ("app.rs", APP),
        ("app_fetch.rs", APP_FETCH),
        ("app_render.rs", APP_RENDER),
        ("app_chunks.rs", APP_CHUNKS),
    ] {
        let n = collapsed(source).matches(SELF_GUI_SET).count();
        assert_eq!(
            n, 0,
            "{name} contains {n} `{SELF_GUI_SET}` push(es) (counted \
             whitespace-collapsed, so a line-wrapped call counts too). \
             WO-E2 replaced the setter push with Gui::apply for event-shaped \
             state and Gui::apply_frame_inputs for frame-composed state — \
             route the new push through the seam.",
        );
    }
}

/// The per-file coupling ceilings. **Permanent, and at their measured values.**
///
/// # These are not migration scaffolding
///
/// They were written as scaffolding and described themselves that way. **User
/// ruling, 2026-08-21: "Keep them, I want loud failures if that contract is
/// broken."** They are not deleted at campaign close or at any milestone. The
/// contract is the one [`SELF_GUI`] names: the app layer does not grow its
/// reach into the UI layer, and an attempt to is a **build failure**, not a
/// review comment.
///
/// # What the numbers are, and what they are not
///
/// **The rule is that each ceiling equals the count it measures, and the land
/// that sheds an occurrence lowers the constant with it.** The values below
/// are the last measurement that satisfied it — re-measured at WO-ARREARS,
/// 2026-08-21, base `178ab361`, by the scrape below. They are not a standing
/// claim about the future: a land that sheds and does not lower leaves
/// **arrears**, which is exactly what happened between WO-E10.4 and here
/// (`1e94ce59` took `app_fetch.rs` 45 -> 42 and left the pin at 45).
///
/// The old message here said WO-E8 would drive them to 0 "when the radar root
/// fields dissolve": E8 landed, the fields dissolved, and these did not reach
/// 0. A ceiling that names a future that already happened is a
/// prose-is-not-evidence defect sitting inside a gate, so it says what is true
/// instead.
///
/// **They may only ever FALL.** The correct response to needing a new reach is
/// **shed first, then land**. The two honest sheds are **loop-state
/// addressing** and **the all-panes-versus-visible-panes distinction**, which
/// has already produced one bug. If neither is reachable inside a change's
/// charter, **stop and report** — never raise, and never re-spell the reads
/// through a local binding, which makes this scrape read zero while the
/// coupling is identical.
///
/// `app_chunks.rs` fell furthest at WO-E10.4 (18 to 13) because its ceiling
/// had been carrying slack since WO-E2; the other three fell by one each.
///
/// # Two of these four files hide reaches from this scrape
///
/// `app.rs` and `app_render.rs` each contain one `let gui = &mut self` +
/// `.gui;` binding — the construct the message below forbids by name. WO-ARREARS
/// **compile-proved neither is borrow-forced** (a direct reach builds with no
/// diagnostic) and measured what they hide: `app.rs` reads 37 here and is
/// really 41, `app_render.rs` reads 101 here and is really 102. Shedding them
/// puts both files above their ceilings, so the shed needs a land that can also
/// shed the difference. The full record, including the crate-wide figure, is
/// on `arch_ratchets.rs`'s `SELF_GUI_MAX`. It is recorded here too because
/// **this is the scrape those two bindings defeat**.
///
/// `app_render.rs` fell 102 -> 101 at WI-6b, which needed two reaches for the
/// overlay loop's dispatch and its arrival and paid for them with three: the
/// index-plus-`pane(idx)` walks in `sync_loop_playback_start` and
/// `dispatch_loop_renders` became slice walks, on WI-0's proof.
#[test]
fn the_gui_coupling_only_ever_shrinks() {
    // Presence control: the scrape reads real, current source. Without it every
    // ceiling below is satisfied by four empty strings.
    assert!(
        collapsed(APP).contains(SELF_GUI_APPLY),
        "control: app.rs no longer contains the seam call — the scrape is not \
         reading the source these ceilings exist to measure",
    );
    for (name, source, ceiling) in [
        ("app.rs", APP, 37),
        ("app_fetch.rs", APP_FETCH, 42),
        ("app_render.rs", APP_RENDER, 101),
        ("app_chunks.rs", APP_CHUNKS, 13),
    ] {
        let n = collapsed(source).matches(SELF_GUI).count();
        assert!(
            n <= ceiling,
            "{name}: {n} `{SELF_GUI}` occurrences > ceiling {ceiling}. This is a \
             PERMANENT contract sitting on its measured value, so there is no \
             slack to spend: shed first, then land. The two honest sheds are \
             loop-state addressing and the all-panes-versus-visible-panes \
             distinction. Lower this in the land that earns it; never raise it \
             without a written plan amendment, and never hide the reads behind \
             a local binding - this scrape would read zero while the coupling \
             is identical.",
        );
    }
}
