//! The basemap's place names are kept as laid-out galleys, and a galley
//! addresses egui's font atlas by pixel position. egui replaces that atlas
//! whole once it passes 80 % full and re-rasterizes every glyph somewhere
//! else. What must never happen is a kept galley being re-served across that
//! event: the positions and advances still come from the font metrics and are
//! right, and only the texels are wrong, so it draws as the right words in the
//! wrong letters.
//!
//! **The property the fixture needs, and it is three things at once.** Enough
//! distinct glyphs to drive egui past the 80 % mark and make it repack; a
//! label laid out *before* that and asked for again *after*; and — the part
//! that took the defect three weeks to be understood — the repack landing on
//! frames the memo was not looking at. A fixture that watches every frame
//! cannot reach this defect and reads green for ever.

fn stamp(ctx: &egui::Context) -> ([usize; 2], f32) {
    ctx.fonts(|f| (f.font_image_size(), f.font_atlas_fill_ratio()))
}

/// Where each glyph of a laid-out label reads the atlas.
fn uvs(g: &egui::Galley) -> Vec<[u16; 2]> {
    g.rows
        .iter()
        .flat_map(|r| r.row.glyphs.iter())
        .map(|gl| gl.uv_rect.min)
        .collect()
}

fn label(name: &str) -> walkers::Text {
    walkers::Text::new(
        egui::pos2(10.0, 10.0),
        name,
        14.0,
        egui::Color32::WHITE,
        0.0,
    )
}

/// One pass, shaped like the shell's: `begin_pass`, then the memo's own check
/// at the head of it before anything has laid text out, then the frame's work.
fn frame<R>(
    ctx: &egui::Context,
    cache: &mut walkers::GalleyCache,
    watched: bool,
    body: impl FnOnce(&egui::Context, &mut walkers::GalleyCache) -> R,
) -> R {
    ctx.begin_pass(egui::RawInput::default());
    if watched {
        cache.begin_frame(ctx);
    }
    let out = body(ctx, cache);
    let _ = ctx.end_pass();
    out
}

/// Rasterize a pile of glyphs, the way panning onto new tiles does. Each size
/// is a fresh set of atlas entries, so this fills the atlas without needing a
/// font that covers a hundred thousand code points.
fn burn(ctx: &egui::Context, size: &mut f32) {
    for _ in 0..4 {
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

/// Grow the atlas to its final height, so a later reading and this one differ
/// in `fill` alone and the size term of the check cannot carry the test.
fn grow_to_full_height(
    ctx: &egui::Context,
    cache: &mut walkers::GalleyCache,
    size: &mut f32,
) -> [usize; 2] {
    for _ in 0..4000 {
        let now = stamp(ctx).0;
        if now[1] == now[0] {
            return now;
        }
        frame(ctx, cache, false, |ctx, _| burn(ctx, size));
    }
    panic!("fixture never grew the atlas to its full height");
}

/// The user's own gesture: a label is drawn, the map is panned and zoomed
/// somewhere else for a while — so this pane never re-solves and the memo's
/// check never runs — and then the map comes back to it.
///
/// Before the memo counted generations this failed, serving the galley laid
/// out under the old atlas: measured `[[404, 1059], ...]` against a truth of
/// `[[351, 1182], ...]`, one repack and one regrowth to the same size at a
/// higher fill apart.
#[test]
fn a_memo_that_missed_the_repack_frames_does_not_serve_stale_glyphs() {
    let ctx = egui::Context::default();
    let mut cache = walkers::GalleyCache::default();
    frame(&ctx, &mut cache, false, |_, _| {});
    let mut size = 6.0f32;

    let full = grow_to_full_height(&ctx, &mut cache, &mut size);

    // On screen: the label enters the memo, on a frame the memo watched.
    let kept = frame(&ctx, &mut cache, true, |ctx, cache| {
        label("Tulsa").galley_cached(ctx, cache)
    });
    let stored = stamp(&ctx);
    assert_eq!(stored.0, full);

    // Away: nothing asks this memo, so nothing checks the atlas. egui repacks
    // it and it regrows, all unobserved.
    let mut repacked = false;
    for _ in 0..4000 {
        let before = stamp(&ctx);
        frame(&ctx, &mut cache, false, |ctx, _| burn(ctx, &mut size));
        let now = stamp(&ctx);
        repacked |= now.1 < before.1;
        if repacked && now.0 == stored.0 && now.1 >= stored.1 {
            break;
        }
    }
    let now = stamp(&ctx);
    assert!(repacked, "fixture never drove a repack");
    assert_eq!(
        now.0, stored.0,
        "fixture must come back to the size it stored under, or the check \
         answers on the size and the defect is never reached"
    );
    assert!(
        now.1 >= stored.1,
        "fixture must come back at or above the fill it stored under, for the \
         same reason: {now:?} against {stored:?}"
    );

    // Back: the same name is asked for again.
    let (served, truth) = frame(&ctx, &mut cache, true, |ctx, cache| {
        (
            label("Tulsa").galley_cached(ctx, cache),
            label("Tulsa").galley(ctx),
        )
    });
    assert_ne!(
        uvs(&kept),
        uvs(&truth),
        "fixture is vacuous: the repack put the glyphs back where they were"
    );
    assert_eq!(
        uvs(&served),
        uvs(&truth),
        "the memo served glyph coordinates from before the repack"
    );
}

/// The same repack, watched at the head of every pass the way the shell now
/// watches it. This is the path production takes, and it is a separate test
/// because it is a different claim: the one above says a memo that *missed*
/// the frames recovers, this says a memo that watched them never had to.
#[test]
fn a_memo_watched_every_pass_does_not_serve_stale_glyphs() {
    let ctx = egui::Context::default();
    let mut cache = walkers::GalleyCache::default();
    frame(&ctx, &mut cache, false, |_, _| {});
    let mut size = 6.0f32;
    grow_to_full_height(&ctx, &mut cache, &mut size);

    let mut repacked = false;
    for _ in 0..4000 {
        let before = stamp(&ctx);
        let (served, truth) = frame(&ctx, &mut cache, true, |ctx, cache| {
            let served = label("Tulsa").galley_cached(ctx, cache);
            let truth = label("Tulsa").galley(ctx);
            burn(ctx, &mut size);
            (served, truth)
        });
        assert_eq!(
            uvs(&served),
            uvs(&truth),
            "stale glyphs under a watched memo"
        );
        repacked |= stamp(&ctx).1 < before.1;
        if repacked {
            break;
        }
    }
    assert!(repacked, "fixture never drove a repack");
}

/// The counter the caches stamp themselves with: it moves when the raster
/// moves, and — the half that keeps the memo worth having — it does not move
/// on a frame where nothing did.
#[test]
fn the_generation_moves_only_when_the_raster_does() {
    let ctx = egui::Context::default();
    let mut cache = walkers::GalleyCache::default();
    frame(&ctx, &mut cache, false, |_, _| {});
    let mut size = 6.0f32;
    grow_to_full_height(&ctx, &mut cache, &mut size);

    // A still map: the same label, frame after frame, moves nothing.
    let settled = frame(&ctx, &mut cache, true, |ctx, cache| {
        label("Tulsa").galley_cached(ctx, cache);
        cache.generation()
    });
    for _ in 0..8 {
        let g = frame(&ctx, &mut cache, true, |ctx, cache| {
            label("Tulsa").galley_cached(ctx, cache);
            cache.generation()
        });
        assert_eq!(g, settled, "a still map must not churn the generation");
    }
    assert!(!cache.is_empty(), "a still map must not empty the memo");

    // A repack does move it.
    let mut repacked = false;
    for _ in 0..4000 {
        let before = stamp(&ctx);
        frame(&ctx, &mut cache, true, |ctx, _| burn(ctx, &mut size));
        if stamp(&ctx).1 < before.1 {
            repacked = true;
            break;
        }
    }
    assert!(repacked, "fixture never drove a repack");
    assert_ne!(
        cache.generation(),
        settled,
        "a repack must move the generation"
    );
}
