//! **Every measurement scene seeds the layout its own header claims.**
//!
//! `rig_seed_tests` reads `run_tier2.sh`'s one seed. Nothing read
//! `run_measure.sh`'s **seven**, and that is where the scoreboard's rows come
//! from. Two ways that was already wrong, both found 2026-08-31:
//!
//! * **Scene C's three statements disagreed.** The seed named six panes across
//!   three sites; its header comment said two sites; and `layer_link` — which
//!   defaults to `true` — collapsed the group onto the *active* pane's site on
//!   the first shell frame, so every scene-C row ever taken measured six panes
//!   on **one** site. Nothing could see it: `load_ui_config` alone reports
//!   three sites, because the convergence happens in
//!   [`Gui::propagate_pane_sync`], which is a shell path and not a load path.
//!   That is why the check below runs it.
//! * **A seed that never reached the browser reads as a valid row.** A leg
//!   pointed at `/index.html` instead of `/index-rig.html` gets no prelude, no
//!   `window.__rig`, no `localStorage` write — the app then opens on a site
//!   derived from the machine's timezone and every figure is against a scene
//!   nobody chose. This file cannot see that (it is the host side); the
//!   browser half is `--expect-seed-applied` in `drive.py`, and the two
//!   together are the chain: **this** proves the literal describes the claimed
//!   scene, and **that** proves a browser applied a literal and did not fall
//!   back.
//!
//! Read out of the shell script rather than restated, on the same terms
//! `rig_seed_tests` reads `run_tier2.sh`: a copy of a literal is a second place
//! for it to be wrong. The seeds here are **shell-spliced** rather than plain
//! literals (`'…'"$PANEL_SEED"'…'`), so [`scene_seed`] performs the splice the
//! same way the shell does, from the fragment variables' own assignment lines.

use super::*;
use crate::Gui;
use crate::gesture_player::GestureScript;
use crate::pane::MapRender;
use squallar_kv::{KvStore, MemoryKvStore};

/// The measurement launcher, read at compile time so a moved or deleted file
/// is a build failure rather than a skipped test.
const RUN_MEASURE: &str = include_str!("../../../.github/browser-rig/run_measure.sh");

/// The `squallar.gesture_script` key's value, or `None` for a scene with no
/// script.
type Script = Option<&'static str>;

/// **What each scene is FOR, stated here and not derived from the seed.**
///
/// This is the whole non-vacuity of the file. A table read out of the seed
/// would agree with the seed by construction — the shape of every check this
/// campaign has had to delete. These rows are the header block's claims
/// (`run_measure.sh`, "The scenes"), transcribed, and the assertions below are
/// the seed being held to them.
///
/// `sites` is the number of **distinct** sites still on screen after the shell
/// has propagated, not the number the seed names. For six layer-linked panes
/// those are different numbers, which is the defect this file was written for.
struct Scene {
    name: &'static str,
    panes: usize,
    sites: usize,
    volume_panes: usize,
    script: Script,
    /// Whether the header claims this scene carries the whole layer stack.
    all_layers: bool,
    /// Whether the header claims this scene's pane is seeded looping.
    looping: bool,
}

const SCENES: &[Scene] = &[
    Scene {
        name: "A",
        panes: 1,
        sites: 1,
        volume_panes: 0,
        script: Some("pan-zoom-2d"),
        all_layers: true,
        looping: false,
    },
    Scene {
        name: "B",
        panes: 1,
        sites: 1,
        volume_panes: 1,
        script: Some("orbit-3d"),
        all_layers: false,
        looping: false,
    },
    Scene {
        name: "C",
        panes: 6,
        sites: 3,
        volume_panes: 3,
        script: Some("pan-zoom-2d"),
        all_layers: false,
        looping: false,
    },
    Scene {
        name: "D",
        panes: 1,
        sites: 1,
        volume_panes: 0,
        script: Some("ui-sweep"),
        all_layers: true,
        looping: false,
    },
    Scene {
        name: "E1",
        panes: 1,
        sites: 1,
        volume_panes: 0,
        script: None,
        all_layers: true,
        looping: true,
    },
    Scene {
        name: "E2",
        panes: 1,
        sites: 1,
        volume_panes: 0,
        script: Some("pan-zoom-2d"),
        all_layers: true,
        looping: true,
    },
    Scene {
        name: "E3",
        panes: 1,
        sites: 1,
        volume_panes: 1,
        script: Some("orbit-3d"),
        // "a volume pane looping" — the header claims no layer stack for E3,
        // and the seed names none. E1/E2 carry `ALL_LAYERS`; this one does not.
        all_layers: false,
        looping: true,
    },
];

/// The scenes a `case` body can answer for, whatever whitespace separates the
/// pattern from its `echo`. The patterns are `A`, or `B|E3` — alternations, so
/// each arm can name several scenes.
fn case_arms(body: &str) -> Vec<&str> {
    body.lines()
        .filter_map(|line| {
            let (pattern, rest) = line.trim().split_once(')')?;
            rest.trim_start().starts_with("echo").then_some(pattern)
        })
        .flat_map(|pattern| pattern.split('|'))
        .collect()
}

/// The value of a one-line shell assignment `NAME='…'` or `NAME=""`.
///
/// Only the **first** assignment is taken, which is the one that runs by
/// default: `PANEL_SEED` is assigned empty at top level and re-assigned inside
/// `if [ "$PANEL" = on ]`, and `RIG_PANEL` defaults to `off`.
fn shell_var(name: &str) -> String {
    let head = format!("\n{name}=");
    let at = RUN_MEASURE.find(&head).unwrap_or_else(|| {
        panic!(
            "run_measure.sh no longer assigns `{name}` at the start of a line; \
             the scene seeds' splice moved and this test can no longer perform it"
        )
    });
    let rest = &RUN_MEASURE[at + head.len()..];
    let line = rest.lines().next().expect("a line follows the assignment");
    for quote in ['\'', '"'] {
        if let Some(body) = line.strip_prefix(quote) {
            return body
                .split_once(quote)
                .unwrap_or_else(|| panic!("`{name}`'s assignment is not closed on its own line"))
                .0
                .to_string();
        }
    }
    panic!("`{name}` is assigned something this test cannot read: {line:?}")
}

/// One scene's whole `localStorage` seed, with the shell's splice performed.
///
/// The arms are `SCENE) echo '…'"$VAR"'…' ;;`. Reproduced here rather than
/// pattern-matched loosely: a chunk this cannot read is a panic naming the
/// scene, never a scene silently skipped.
fn scene_seed(scene: &str) -> serde_json::Value {
    let head = format!("\n    {scene}) echo ");
    let at = RUN_MEASURE.find(&head).unwrap_or_else(|| {
        panic!(
            "run_measure.sh's `scene_seed` no longer has an arm for scene \
             {scene}. Either the scene was removed — in which case its row \
             above must go too — or the arm was respelled and every seed in \
             this file is now unread"
        )
    });
    let word = RUN_MEASURE[at + head.len()..]
        .lines()
        .next()
        .expect("a line follows the arm")
        .strip_suffix(" ;;")
        .unwrap_or_else(|| panic!("scene {scene}'s arm does not end on its own line"));

    let mut out = String::new();
    let mut rest = word;
    while !rest.is_empty() {
        if let Some(body) = rest.strip_prefix('\'') {
            let (chunk, after) = body
                .split_once('\'')
                .unwrap_or_else(|| panic!("scene {scene}: an unclosed '…' chunk"));
            out.push_str(chunk);
            rest = after;
        } else if let Some(body) = rest.strip_prefix('"') {
            let (chunk, after) = body
                .split_once('"')
                .unwrap_or_else(|| panic!("scene {scene}: an unclosed \"…\" chunk"));
            // `"$A"` and `"$A$B"` are the only forms the script uses.
            for name in chunk.split('$').skip(1) {
                out.push_str(&shell_var(name));
            }
            assert!(
                chunk.starts_with('$'),
                "scene {scene}: a double-quoted chunk that is not a splice: {chunk:?}",
            );
            rest = after;
        } else {
            panic!("scene {scene}: unquoted shell text this test cannot expand: {rest:?}");
        }
    }
    serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("scene {scene}'s seed is not valid JSON: {e}\n{out}"))
}

/// The `Gui` a browser really has once this scene's seed is loaded **and the
/// shell has run one frame's worth of propagation**.
fn gui_for(scene: &str) -> Gui {
    let seed = scene_seed(scene);
    let ui = seed
        .get("squallar.ui")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("scene {scene}'s seed carries no `squallar.ui` string"));
    let store = MemoryKvStore::default();
    store
        .store(UI_CONFIG_KEY, ui)
        .expect("the memory store always accepts a write");
    let mut gui = Gui::new();
    assert!(
        gui.load_ui_config(&store),
        "scene {scene}'s seed does not parse as a config any more, so that \
         whole scene's rows are measured on a timezone-derived default and \
         nothing in the row says so",
    );
    // **The step that makes this test about the browser's layout rather than
    // the file's.** `render_stack_and_inspector` calls this once per frame, so
    // a seeded scene is in this state before the settle window opens.
    gui.propagate_pane_sync();
    gui
}

/// **Every scene's seed, held to the layout its header claims.**
#[test]
fn every_measure_scene_seeds_the_layout_it_claims() {
    for scene in SCENES {
        let name = scene.name;
        let gui = gui_for(name);

        assert_eq!(
            gui.pane_count(),
            scene.panes,
            "scene {name} builds {} panes where its header claims {}",
            gui.pane_count(),
            scene.panes,
        );

        let mut sites: Vec<&str> = gui.panes().iter().map(|p| p.site()).collect();
        sites.sort_unstable();
        sites.dedup();
        assert_eq!(
            sites.len(),
            scene.sites,
            "scene {name} displays {} distinct sites ({sites:?}) where its \
             header claims {}. A seed that names more sites than this reports \
             has been collapsed by `layer_link`, which defaults to true and \
             converges every linked pane onto the active pane's site",
            sites.len(),
            scene.sites,
        );

        let volume = gui
            .panes()
            .iter()
            .filter(|p| p.map_render() == Some(MapRender::Volume))
            .count();
        assert_eq!(
            volume, scene.volume_panes,
            "scene {name} draws {volume} volume panes where its header claims {}",
            scene.volume_panes,
        );

        let seed = scene_seed(name);
        let script = seed.get("squallar.gesture_script").and_then(|v| v.as_str());
        assert_eq!(
            script, scene.script,
            "scene {name} arms {script:?} where its header claims {:?}",
            scene.script,
        );
        if let Some(script) = script {
            assert!(
                GestureScript::from_name(script).is_some(),
                "scene {name} arms {script:?}, which this build has no script \
                 for: `GesturePlayer::from_name` logs a warning, disarms, and \
                 the leg measures an idle page while every row still prints",
            );
        }

        // The telemetry keys both halves of every row are scraped from. A
        // scene missing one reports its figures as `null`, which is what the
        // rig also reports when the path never ran.
        for key in ["squallar.frame_telemetry", "squallar.raster_telemetry"] {
            assert_eq!(
                seed.get(key).and_then(|v| v.as_str()),
                Some("1"),
                "scene {name} does not seed {key}, so its lines are written at \
                 `debug`, dropped before the console ring, and the row reads \
                 as a path that never ran",
            );
        }

        if scene.all_layers {
            let stack: Vec<_> = gui
                .overlays
                .handlers()
                .map(|h| h.id())
                .filter(|id| gui.pane(0).expect("pane 0").is_overlay_enabled(id))
                .collect();
            assert!(
                stack.len() >= 17,
                "scene {name} claims the whole layer stack and switches on {} \
                 layers. A respelled layer id parses cleanly and enables \
                 nothing, and the row still prints",
                stack.len(),
            );
        }

        // The E scenes exist to measure a pane that is ANIMATING. The seed
        // says so with a string, and a string this build no longer recognises
        // leaves the pane idle while every row still prints — so both halves
        // are checked: the seed carries the value, and the value is an arm.
        let ui = seed
            .get("squallar.ui")
            .and_then(|v| v.as_str())
            .expect("checked in `gui_for`");
        assert_eq!(
            ui.contains(r#""loop_playback":"playing""#),
            scene.looping,
            "scene {name}: its header claims looping={} and its seed says \
             otherwise",
            scene.looping,
        );
        if scene.looping {
            assert!(
                loop_arm_from_config(Some("playing")).is_some(),
                "`playing` is no longer a loop arm this build knows, so every \
                 E scene's seed leaves its pane idle and the loop rows measure \
                 what scenes A..D already cover",
            );
        }
    }
}

/// **The floor: the table above covers every arm the script really has.**
///
/// Without this, a scene added to `scene_seed` and never added here is
/// measured, scored and never checked — and the test above stays green,
/// because it only walks its own list. `SCENES` is the assertion; this is what
/// stops the assertion being about a subset nobody stated.
#[test]
fn the_scene_table_names_every_arm_run_measure_can_seed() {
    let (_, body) = RUN_MEASURE
        .split_once("scene_seed() {")
        .expect("run_measure.sh no longer defines `scene_seed`");
    let body = body
        .split_once("\n}")
        .expect("`scene_seed` has no recognisable body")
        .0;

    let arms = case_arms(body);
    assert!(
        !arms.is_empty(),
        "no `<scene>) echo …` arm was read out of `scene_seed`, so the check \
         below compares two empty sets and every scene is unchecked",
    );
    let mut missing: Vec<&str> = arms
        .iter()
        .copied()
        .filter(|arm| !SCENES.iter().any(|s| s.name == *arm))
        .collect();
    missing.sort_unstable();
    assert!(
        missing.is_empty(),
        "run_measure.sh can seed {missing:?}, and this file states no expected \
         layout for them: those scenes are measured and scored with nothing \
         holding their seed to what their header claims",
    );

    let mut extra: Vec<&str> = SCENES
        .iter()
        .map(|s| s.name)
        .filter(|name| !arms.contains(name))
        .collect();
    extra.sort_unstable();
    assert!(
        extra.is_empty(),
        "this file expects scenes {extra:?} that `scene_seed` cannot produce",
    );

    // `scene_script` must answer for every arm too: a scene it has no row for
    // returns the empty string, and the row's `script=` denominator is blank
    // while the leg really runs whatever the seed armed.
    let (_, scripts) = RUN_MEASURE
        .split_once("scene_script() {")
        .expect("run_measure.sh no longer defines `scene_script`");
    let scripts = scripts
        .split_once("\n}")
        .expect("`scene_script` has no recognisable body")
        .0;
    let script_arms = case_arms(scripts);
    assert!(
        !script_arms.is_empty(),
        "no arm was read out of `scene_script`, so the loop below compares \
         every scene against an empty set and passes on nothing",
    );
    for scene in SCENES {
        assert!(
            script_arms.contains(&scene.name),
            "`scene_script` has no arm matching scene {}, so the row's \
             `script=` column is blank for a leg that really ran one",
            scene.name,
        );
    }
}

/// **Scene C's header says what its seed does.**
///
/// The one place prose is pinned in this file, and it is pinned because prose
/// is exactly what was wrong: the header said "two sites" while the seed named
/// three and the shell displayed one. A number in a comment that no test reads
/// is a claim with nothing behind it.
#[test]
fn scene_c_header_states_the_site_count_its_seed_produces() {
    let line = RUN_MEASURE
        .lines()
        .find(|l| l.trim_start().starts_with("#   C  "))
        .expect("run_measure.sh's scene block no longer describes scene C");
    assert!(
        line.contains("THREE sites"),
        "scene C's header does not state the three sites its seed now \
         produces: {line:?}",
    );
    assert!(
        !line.contains("two sites"),
        "scene C's header still says two sites: {line:?}",
    );
}
