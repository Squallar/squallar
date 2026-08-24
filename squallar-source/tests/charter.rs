//! The crate's charter, held as tests: a dependency ceiling and the graph
//! shape it exists to create.
//!
//! All read `cargo metadata --no-deps --format-version 1`, whose
//! `packages[].dependencies` are *declared* dependencies, so no feature
//! selection can mask what these assert. One name may appear once per kind,
//! so entries are judged per `(kind, name)`.
//!
//! The absence rules are **graph** rules, not name rules. Asking only whether
//! one manifest spells another crate's name proves nothing about what the
//! compiler is handed: an indirect route restores the whole of the forbidden
//! crate while the direct-edge question still answers "no". The absence tests
//! below therefore walk the transitive closure, and each carries a control
//! that fails if the walk stops recursing.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("squallar-source sits one level under the workspace root")
        .to_path_buf()
}

/// The declared-dependency metadata of every workspace member, as parsed JSON.
fn metadata() -> serde_json::Value {
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(workspace_root())
        .output()
        .expect("cargo metadata should run");
    assert!(
        out.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    serde_json::from_slice(&out.stdout).expect("cargo metadata emits valid JSON")
}

/// `(kind, name)` for every dependency `package` declares. `kind: null` is a
/// normal dependency; target-gated entries are included.
fn declared_deps(meta: &serde_json::Value, package: &str) -> BTreeSet<(String, String)> {
    let packages = meta["packages"]
        .as_array()
        .expect("metadata carries a packages array");
    let found = packages
        .iter()
        .find(|p| p["name"].as_str() == Some(package))
        .unwrap_or_else(|| panic!("workspace member `{package}` is missing from cargo metadata"));
    found["dependencies"]
        .as_array()
        .expect("a package carries a dependencies array")
        .iter()
        .map(|d| {
            let kind = d["kind"].as_str().unwrap_or("normal").to_string();
            let name = d["name"]
                .as_str()
                .expect("a dependency has a name")
                .to_string();
            (kind, name)
        })
        .collect()
}

/// Every hop between workspace members, as `member -> {(kind, member)}`.
///
/// Edges pointing outside the workspace are dropped. That loses nothing for a
/// first-party target: a registry crate cannot name a path dependency, so no
/// route from one workspace member to another can leave the workspace and come
/// back. For a *third-party* target (the GUI rule below) the walk is exact only
/// for routes whose last hop leaves a workspace member — which is every route
/// this workspace controls.
fn member_graph(meta: &serde_json::Value) -> BTreeMap<String, BTreeSet<(String, String)>> {
    let packages = meta["packages"]
        .as_array()
        .expect("metadata carries a packages array");
    let members: BTreeSet<&str> = packages.iter().filter_map(|p| p["name"].as_str()).collect();
    packages
        .iter()
        .map(|p| {
            let name = p["name"]
                .as_str()
                .expect("a package has a name")
                .to_string();
            let edges = p["dependencies"]
                .as_array()
                .expect("a package carries a dependencies array")
                .iter()
                .filter_map(|d| {
                    let dep = d["name"].as_str().expect("a dependency has a name");
                    members.contains(dep).then(|| {
                        (
                            d["kind"].as_str().unwrap_or("normal").to_string(),
                            dep.to_string(),
                        )
                    })
                })
                .collect();
            (name, edges)
        })
        .collect()
}

/// Every workspace member reachable from `root`, mapped to the route that
/// reaches it. `root` itself is never a key.
///
/// `first_hop_kinds` selects which of `root`'s own edges may be entered.
/// **Every hop after the first must be `normal`**, because Cargo does not make
/// dev- and build-dependencies transitive: a crate pulled into the closure
/// brings only *its* normal dependencies with it. Passing `["normal"]` therefore
/// asks what the shipped library compiles against; adding `dev`/`build` asks
/// what any target of `root` compiles against.
///
/// Breadth-first, so a route is one of the shortest, and `normal` is entered
/// before the other kinds so a reported first hop understates nothing.
///
/// **Where this function's own control lives**: every caller asserts its
/// closure is non-empty, but only `the_overlays_to_radar_edge_stays_cut`
/// asserts that the walk *recurses* — it requires a route of more than one hop
/// to a crate reachable no other way. That one control covers all three
/// callers, because they share this function; if it is ever moved, it does not
/// become optional.
fn reachable_from(
    graph: &BTreeMap<String, BTreeSet<(String, String)>>,
    root: &str,
    first_hop_kinds: &[&str],
) -> BTreeMap<String, Vec<(String, String)>> {
    assert!(
        graph.contains_key(root),
        "`{root}` is not a workspace member, so this walk would silently \
         report an empty closure",
    );
    let mut routes: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut queue: VecDeque<String> = VecDeque::from([root.to_string()]);
    while let Some(node) = queue.pop_front() {
        let here = routes.get(&node).cloned().unwrap_or_default();
        let Some(edges) = graph.get(&node) else {
            continue;
        };
        // `normal` sorts after `build` and `dev`, so walk it first by hand.
        let ordered = edges
            .iter()
            .filter(|(kind, _)| kind == "normal")
            .chain(edges.iter().filter(|(kind, _)| kind != "normal"));
        for (kind, dep) in ordered {
            let permitted = if node == root {
                first_hop_kinds.contains(&kind.as_str())
            } else {
                kind == "normal"
            };
            if !permitted || dep == root || routes.contains_key(dep) {
                continue;
            }
            let mut route = here.clone();
            route.push((kind.clone(), dep.clone()));
            routes.insert(dep.clone(), route);
            queue.push_back(dep.clone());
        }
    }
    routes
}

/// A route rendered for a failure message: `a -> (normal) b -> (dev) c`.
fn render_route(root: &str, route: &[(String, String)]) -> String {
    let mut out = root.to_string();
    for (kind, name) in route {
        out.push_str(&format!(" -> ({kind}) {name}"));
    }
    out
}

/// The substrate stays a substrate: its dependency set may not grow past the
/// charter. `[dependencies]` ⊆ {squallar-geo, squallar-units, chrono, serde,
/// serde_json, web-time, reqwest, rustls, log}; dev additionally {tokio}; no
/// build deps. `serde_json` is a normal dependency, the `SourceHandler`
/// config-persistence hooks being typed on `serde_json::Value`.
///
/// ⊆ and not equality on purpose. The floor assertion at the bottom is what
/// keeps this test falsifiable — an empty parse cannot pass it.
#[test]
fn the_dependency_ceiling_holds() {
    const NORMAL_CEILING: &[&str] = &[
        "squallar-geo",
        "squallar-units",
        "chrono",
        "serde",
        "serde_json",
        "web-time",
        "reqwest",
        "rustls",
        "log",
    ];
    const DEV_EXTRA: &[&str] = &["tokio"];

    let meta = metadata();
    let deps = declared_deps(&meta, "squallar-source");

    for (kind, name) in &deps {
        let allowed: bool = match kind.as_str() {
            "normal" => NORMAL_CEILING.contains(&name.as_str()),
            "dev" => NORMAL_CEILING.contains(&name.as_str()) || DEV_EXTRA.contains(&name.as_str()),
            other => panic!(
                "squallar-source declares a `{other}` dependency on {name}; \
                 the charter permits none"
            ),
        };
        assert!(
            allowed,
            "squallar-source declares {name} ({kind}), which is outside the \
             charter ceiling. The substrate is contract/vocabulary only — if a \
             step genuinely needs this, the charter in this test and the plan \
             must change first, in writing.",
        );
    }

    // Falsifiability floor: the crate really declares dependencies.
    assert!(
        deps.iter().any(|(k, n)| k == "normal" && n == "reqwest"),
        "squallar-source no longer declares reqwest — either the crate changed \
         shape or this test is reading the wrong package: {deps:?}",
    );

    // The ceiling above is an allow-list over *declared* edges, so an
    // allow-listed crate that itself grew a first-party dependency would raise
    // the substrate without moving a single line of this list. The first-party
    // half of the ceiling is therefore also asserted over the closure. It is
    // deliberately only the first-party half: the third-party closure of
    // reqwest is not a thing this workspace decides.
    let closure = reachable_from(
        &member_graph(&meta),
        "squallar-source",
        &["normal", "dev", "build"],
    );
    let outside: Vec<_> = closure
        .keys()
        .filter(|name| !NORMAL_CEILING.contains(&name.as_str()))
        .collect();
    assert!(
        outside.is_empty(),
        "squallar-source reaches the workspace members {outside:?}, which are \
         outside the charter ceiling even though nothing in its own manifest \
         names them. The substrate is contract/vocabulary only, and it is what \
         it *compiles*, not what it spells.",
    );
    assert!(
        closure.contains_key("squallar-geo"),
        "the closure of squallar-source is missing squallar-geo, so the check \
         above is reading nothing: {closure:?}",
    );
}

/// **Neither layer crate declares a GUI dependency, of any kind.**
///
/// The rule is older than this test and was held only by reading the manifests:
/// `squallar-overlays` and `squallar-source` are contract, vocabulary and
/// behaviour, and nothing in them may name the toolkit. `squallar-source`'s
/// charter ceiling above is an allowlist and already refuses it; overlays had
/// no ceiling at all, so this is where the rule becomes a test instead of a
/// grep somebody once ran. WO-E10.4.
///
/// The floor is the falsifiability half: both crates must declare *something*,
/// or "declares no egui" is true of an empty list.
///
/// Graph-shaped for the same reason the radar cut is: a layer crate that picks
/// up `squallar-egui` has the toolkit in its build whether or not its own
/// manifest ever spells `egui`. So the needles are checked against each root
/// **and every workspace member in its closure**, which is where a real
/// regression would come from — `squallar-egui` is the crate that declares
/// `egui`, and the day either layer crate reaches it, this goes red.
#[test]
fn no_layer_crate_declares_a_gui_dependency() {
    const GUI_CRATES: &[&str] = &["egui", "eframe", "epaint", "emath", "egui-winit"];
    let meta = metadata();
    let graph = member_graph(&meta);

    for package in ["squallar-overlays", "squallar-source"] {
        let deps = declared_deps(&meta, package);
        assert!(
            !deps.is_empty(),
            "{package} declares no dependencies at all, so every absence below \
             is the reader's and not the manifest's",
        );
        let closure = reachable_from(&graph, package, &["normal", "dev", "build"]);
        assert!(
            closure.contains_key("squallar-geo"),
            "the closure of {package} does not contain squallar-geo, so every \
             absence below is the walk's and not the manifest's: {:?}",
            closure.keys().collect::<Vec<_>>(),
        );

        for member in std::iter::once(package.to_string()).chain(closure.keys().cloned()) {
            let gui: Vec<_> = declared_deps(&meta, &member)
                .into_iter()
                .filter(|(_, name)| GUI_CRATES.contains(&name.as_str()))
                .collect();
            let route = if member == package {
                package.to_string()
            } else {
                render_route(package, &closure[&member])
            };
            assert!(
                gui.is_empty(),
                "{member} declares {gui:?}, and {package} reaches it:\n    \
                 {route}\nThese crates are the layer contract and the \
                 vocabulary under it; a handler that can name the toolkit can \
                 draw, and then the seam is no longer the only way in. \
                 Anything genuinely shared belongs on the trait or in \
                 squallar-source's own types.",
            );
        }
    }

    // Presence control on the needles themselves: a renamed or dropped GUI
    // crate would leave every absence above trivially true. squallar-egui is
    // outside both closures and must still declare one of these names.
    let toolkit: Vec<_> = declared_deps(&meta, "squallar-egui")
        .into_iter()
        .filter(|(_, name)| GUI_CRATES.contains(&name.as_str()))
        .collect();
    assert!(
        !toolkit.is_empty(),
        "squallar-egui declares none of {GUI_CRATES:?}, so the needle list has \
         rotted and the absences above prove nothing",
    );
}

/// **The two data crates cannot reach each other by any route.**
///
/// Both directions, though the name records the one that matters: the test as
/// written only ever asked the overlays side, so squallar-radar could have
/// declared squallar-overlays with nothing going red.
///
/// Not "squallar-overlays does not name squallar-radar" — that is the question
/// this test used to ask, and it was the wrong one. A name check is
/// direct-edge-only, so `squallar-overlays -> X -> squallar-radar` restores the
/// entire nexrad pipeline into every overlay handler while the check reads
/// green. The route is not hypothetical: `squallar-device-profile` declares
/// `squallar-radar`, so a single new edge from the overlays side to the device
/// profile would have been enough.
///
/// Two closures, because their severities differ. The `normal`-only one is what
/// the shipped library compiles against and is the charter proper. The
/// any-kind one adds squallar-overlays' own dev- and build-dependencies: a route
/// that opens only there does not put radar code in the shipped overlay, but it
/// does put it in front of the compiler, and the rule this test replaced
/// forbade it, so it stays forbidden.
///
/// The controls are the point. Three of them: the closure is non-empty and
/// holds a crate squallar-overlays really does stand on; a walk from `squallar`
/// must *find* squallar-radar by a route of more than one hop, which only a
/// walk that recurses can do; and both directions of the squallar-source floor.
#[test]
fn the_overlays_to_radar_edge_stays_cut() {
    let meta = metadata();
    let graph = member_graph(&meta);

    // Both directions. The charter sentence is "they do not know about each
    // other", and until this test grew its second arm only the overlays side
    // was ever asserted — squallar-radar was free to declare squallar-overlays
    // with nothing going red.
    for (from, to) in [
        ("squallar-overlays", "squallar-radar"),
        ("squallar-radar", "squallar-overlays"),
    ] {
        let shipped = reachable_from(&graph, from, &["normal"]);
        assert!(
            !shipped.contains_key(to),
            "{to} is reachable from {from}:\n    {}\nEvery {from} handler now \
             compiles against the whole of {to}. That edge is the one the layer \
             split exists to remove; anything both sides need belongs in \
             squallar-source instead.",
            render_route(from, &shipped[to]),
        );

        let any_kind = reachable_from(&graph, from, &["normal", "dev", "build"]);
        assert!(
            !any_kind.contains_key(to),
            "{to} is reachable from {from} through one of {from}'s own dev- or \
             build-dependencies:\n    {}\nThis does not ship inside {from}, but \
             its test and build targets compile {to}, and the charter forbids \
             the edge in every kind.",
            render_route(from, &any_kind[to]),
        );

        // Falsifiability floor: the closure is a real closure, not an empty set.
        assert!(
            shipped.contains_key("squallar-source"),
            "the closure of {from} does not contain squallar-source, so the \
             absence above is the walk's and not the manifest's: {:?}",
            shipped.keys().collect::<Vec<_>>(),
        );
    }

    // Transitivity control: `squallar` names none of the pipeline crates, and
    // reaches squallar-radar only through squallar-app. A walk that read direct
    // edges alone — the bug this test was written to close — finds nothing
    // here, so this assertion is exactly the one the old check failed.
    let entry = reachable_from(&graph, "squallar", &["normal", "dev", "build"]);
    let indirect = entry
        .get("squallar-radar")
        .expect("squallar reaches squallar-radar through squallar-app");
    assert!(
        indirect.len() > 1,
        "the walk reached squallar-radar from squallar in one hop ({indirect:?}), \
         so it is reading declared edges rather than the closure",
    );
    assert!(
        !declared_deps(&meta, "squallar")
            .iter()
            .any(|(_, name)| name == "squallar-radar"),
        "squallar now declares squallar-radar directly, which retires this \
         control — point it at another crate that is reachable only \
         indirectly, do not delete it",
    );

    let overlays = declared_deps(&meta, "squallar-overlays");
    let radar = declared_deps(&meta, "squallar-radar");
    assert!(
        overlays
            .iter()
            .any(|(kind, name)| kind == "normal" && name == "squallar-source"),
        "squallar-overlays no longer stands on squallar-source: {overlays:?}",
    );
    assert!(
        radar
            .iter()
            .any(|(kind, name)| kind == "normal" && name == "squallar-source"),
        "squallar-radar no longer stands on squallar-source: {radar:?}",
    );
}
