//! **The frame drive is what watches the glyph raster, and nothing else does.**
//!
//! Every cache in `squallar-egui` that holds baked font-atlas coordinates —
//! the pane's label solves, the point pass's tessellated text, and the galley
//! memo under both — keys itself on `Gui::atlas_generation`, and the one call
//! that moves it is the unconditional `GalleyCache::begin_frame` at the head
//! of `Gui::ui_phased`. Take it away and the counter never leaves zero: every
//! key matches for ever and the map re-paints label geometry across an egui
//! atlas repack, which draws the right words in the wrong letters.
//!
//! Its own test binary, and that is not tidiness: driving whole frames writes
//! `squallar_egui::tile_mesh::ledger`, which is a process-global that the
//! crate's own unit tests bracket. In here it shares a process with nothing.

/// Rasterize a pile of glyphs, the way panning onto new tiles does. Each size
/// is a fresh set of atlas entries, so this fills the atlas without needing a
/// font that covers a hundred thousand code points.
fn burn(ctx: &egui::Context, size: &mut f32) {
    for _ in 0..20 {
        *size += 0.25;
        let mut job = egui::text::LayoutJob::default();
        job.append(
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(*size),
                color: egui::Color32::WHITE,
                ..Default::default()
            },
        );
        ctx.fonts_mut(|f| f.layout_job(job));
    }
}

fn fill(ctx: &egui::Context) -> f32 {
    ctx.fonts(|f| f.font_atlas_fill_ratio())
}

#[test]
fn a_frame_drive_notices_the_atlas_being_repacked() {
    let ctx = egui::Context::default();
    let mut gui = squallar_egui::Gui::new();
    let mut size = 6.0f32;

    let mut frame = |gui: &mut squallar_egui::Gui, burn_glyphs: bool| {
        ctx.begin_pass(egui::RawInput::default());
        gui.ui(&ctx);
        if burn_glyphs {
            burn(&ctx, &mut size);
        }
        let _ = ctx.end_pass();
    };

    frame(&mut gui, false);
    let settled = gui.atlas_generation();

    // Frames that rasterize nothing new must not churn the counter, or the
    // memos it guards are emptied every frame and the guard costs more than
    // the defect it closes.
    for _ in 0..4 {
        frame(&mut gui, false);
    }
    assert_eq!(
        gui.atlas_generation(),
        settled,
        "a still frame must not move the atlas generation"
    );

    let mut repacked = false;
    for _ in 0..600 {
        let before = fill(&ctx);
        frame(&mut gui, true);
        if fill(&ctx) < before {
            repacked = true;
            // One more: egui repacks at the head of a pass, and the head of
            // the *next* pass is where the drive reads it.
            frame(&mut gui, false);
            break;
        }
    }
    assert!(repacked, "fixture never drove a repack");
    assert_ne!(
        gui.atlas_generation(),
        settled,
        "the frame drive did not notice the atlas being repacked"
    );
}
