//! **The Tier-2 rig's scene seed still seeds what it says it seeds.**
//!
//! `.github/browser-rig/run_tier2.sh` pins the browser scene by writing one
//! `localStorage` string before any app script runs, and that string is
//! deliberately in the OLDEST config shape so every migration rung runs on
//! every Tier-2 leg. Which makes it exactly the kind of literal that rots
//! silently: a rung that stopped consuming `enabled_overlays`, a layer id that
//! was respelled, a handler whose `default_enabled` moved — any of them would
//! leave the seed parsing cleanly and enabling nothing.
//!
//! What that costs is not a red gate but a *quiet* one. `--expect-overlay-
//! rasters` would go red with `dispatched == 0`, which is the same reading a
//! genuinely broken overlay pipeline gives, and the two would be
//! indistinguishable from CI output. This test is what separates them: it
//! fails here, on the host, naming the seed.
//!
//! Read out of the shell script itself rather than restated, on the same terms
//! `rustdar-web`'s `pwa_assets` reads `worker_port.rs`: a copy of a literal is
//! a second place for it to be wrong.

use super::*;
use crate::Gui;
use rustdar_kv::{KvStore, MemoryKvStore};

/// The rig script, read at compile time so a moved or deleted file is a build
/// failure rather than a skipped test.
const RUN_TIER2: &str = include_str!("../../../.github/browser-rig/run_tier2.sh");

/// The layer the seed switches on. A texture-mode layer that needs **no
/// network** — the site table is compiled into `rustdar-radar` — and that ships
/// `default_enabled() == false`, which is what makes removing it from the seed
/// a real negative control for the rig gate rather than a tamper.
const SEEDED: rustdar_source::id::LayerId = rustdar_source::id::known::RADAR_SITES;

/// The `SEED_LS` assignment's value, with the shell quoting undone.
///
/// `SEED_LS='{"rustdar.ui": "…\"…\"…"}'` — single-quoted in the script, so the
/// only escaping inside is JSON's own. This returns the *inner* config string,
/// i.e. what the app's `KvStore` is handed under `UI_CONFIG_KEY`.
fn seeded_config_json() -> String {
    let line = RUN_TIER2
        .lines()
        .find(|l| l.starts_with("SEED_LS='"))
        .expect(
            "run_tier2.sh no longer has a line starting `SEED_LS='`; the rig's \
             scene seed moved and this test can no longer read it",
        );
    let body = line
        .trim_start_matches("SEED_LS='")
        .strip_suffix('\'')
        .expect("the SEED_LS assignment is not closed on its own line");
    let outer: serde_json::Value = serde_json::from_str(body).expect("the seed is not valid JSON");
    outer
        .get("rustdar.ui")
        .and_then(|v| v.as_str())
        .expect("the seed no longer carries a `rustdar.ui` string")
        .to_string()
}

/// **The seed the browser rig writes really produces a pane with a texture
/// overlay switched on.**
///
/// Three assertions, and the second and third are what stop this passing
/// vacuously. The seed could parse into a pane with no layers at all and the
/// first would still hold; it could switch on a layer that draws as points or
/// tiles, which never reaches the overlay texture dispatch and would leave the
/// rig's raster counters at zero with everything looking enabled.
#[test]
fn the_browser_rigs_scene_seed_enables_a_texture_overlay() {
    let store = MemoryKvStore::default();
    store
        .store(UI_CONFIG_KEY, &seeded_config_json())
        .expect("the memory store always accepts a write");

    let mut gui = Gui::new();
    assert!(
        gui.load_ui_config(&store),
        "the rig's own scene seed does not parse as a config any more",
    );

    let pane = gui.pane(0).expect("the seed asks for one pane");
    assert_eq!(
        pane.site(),
        "KTLX",
        "the seed's radar site did not survive the migration chain, so the \
         browser legs are not on the scene the script says they are",
    );
    assert!(
        pane.is_overlay_enabled(&SEEDED),
        "the rig seed no longer switches {SEEDED:?} on. `--expect-overlay-rasters` \
         will read `dispatched == 0` and go red in a way that is \
         indistinguishable from a genuinely broken overlay pipeline",
    );
    assert_eq!(
        gui.overlays.render_mode(&SEEDED),
        Some(rustdar_source::handler::RenderMode::Texture),
        "{SEEDED:?} no longer rasterizes to a texture, so switching it on seeds \
         no overlay upload at all and the rig's raster counters would stay at \
         zero with the layer looking enabled",
    );
}

/// **The seeded layer is a real choice, not a restatement of the default —
/// and removing it is NOT the rig's negative control.**
///
/// The distinction cost a browser run to learn. `RadarSites` being off by
/// default is what makes seeding it mean something. But a pane without it
/// still has texture overlays: `NwsAlerts` and `SpcDiscussions` ship
/// `default_enabled() == true` and both rasterize, so a scene with the key
/// removed still reaches the overlay dispatch whenever the live feeds have
/// anything in them — measured 2026-08-22, chromium reached 2 dispatched /
/// 2 pictures / 16512000 B that way. What the seed buys is therefore not "the
/// only texture overlay" but "the only one that does not depend on the
/// weather", and the control that really goes red is every texture layer
/// switched explicitly off. Both halves are asserted here, because the second
/// is the one that would silently turn the rig's control sentence into a lie.
#[test]
fn the_seeded_layer_is_one_a_fresh_pane_would_not_have() {
    let store = MemoryKvStore::default();
    // The seed with the layer key taken back out.
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":1,"panes":[{"site":"KTLX"}]}"#,
        )
        .expect("the memory store always accepts a write");

    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store));
    assert!(
        !gui.pane(0)
            .expect("the seed asks for one pane")
            .is_overlay_enabled(&SEEDED),
        "{SEEDED:?} is on for a pane that never asked for it, so seeding it \
         changes nothing and the rig's determinism claim rests on nothing",
    );

    let texture_layers: Vec<_> = gui
        .overlays
        .handlers()
        .filter(|h| h.render_mode() == rustdar_source::handler::RenderMode::Texture)
        .map(|h| h.id())
        .filter(|id| *id != rustdar_source::id::known::RADAR)
        .collect();
    let pane = gui.pane(0).expect("the seed asks for one pane");
    let still_on: Vec<_> = texture_layers
        .iter()
        .filter(|id| pane.is_overlay_enabled(id))
        .collect();
    assert!(
        !still_on.is_empty(),
        "a pane without the rig's seed has no texture overlay left at all. That \
         would make *dropping the seed* a valid negative control for \
         --expect-overlay-rasters, and both run_tier2.sh and drive.py currently \
         say in as many words that it is not one",
    );
}
