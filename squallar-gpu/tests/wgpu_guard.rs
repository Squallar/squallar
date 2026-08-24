//! Gates the shape of the single-copy wgpu guard in `src/lib.rs`. The guard's
//! own `on_unimplemented` notes say what it is for.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use syn::visit::Visit;
use syn::{Expr, GenericArgument, Item, PathArguments, Stmt, Type, UseTree};

/// A type path reduced to the two things the guard depends on.
#[derive(PartialEq, Eq, Debug)]
struct CratePath {
    /// `::`-rooted, so unambiguously a crate rather than something `use`d.
    rooted: bool,
    segments: Vec<String>,
}

impl std::fmt::Display for CratePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}",
            if self.rooted { "::" } else { "" },
            self.segments.join("::")
        )
    }
}

/// Type arguments of every turbofish in an expression, so the guard may spell
/// its assertion as a call, an `fn()` coercion, or anything else.
#[derive(Default)]
struct TurbofishArgs<'ast>(Vec<&'ast Type>);

impl<'ast> Visit<'ast> for TurbofishArgs<'ast> {
    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        for seg in &node.path.segments {
            if let PathArguments::AngleBracketed(args) = &seg.arguments {
                self.0.extend(args.args.iter().filter_map(|a| match a {
                    GenericArgument::Type(t) => Some(t),
                    _ => None,
                }));
            }
        }
        syn::visit::visit_expr_path(self, node);
    }
}

/// Follow `type X = Y;` aliases declared in the guard until a real path is hit.
fn resolve<'a>(mut ty: &'a Type, aliases: &HashMap<String, &'a Type>) -> &'a Type {
    for _ in 0..16 {
        let Type::Path(p) = ty else { return ty };
        if p.qself.is_some() || p.path.leading_colon.is_some() || p.path.segments.len() != 1 {
            return ty;
        }
        match aliases.get(&p.path.segments[0].ident.to_string()) {
            Some(next) => ty = next,
            None => return ty,
        }
    }
    panic!("type alias cycle in the wgpu guard");
}

fn crate_path(ty: &Type) -> CratePath {
    let Type::Path(p) = ty else {
        panic!("the wgpu guard compares a non-path type")
    };
    CratePath {
        rooted: p.path.leading_colon.is_some(),
        segments: p
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect(),
    }
}

/// The two types the guard compares: the one the marker trait is implemented
/// for, and the one the assertion requires it of.
fn guard_sides() -> (CratePath, CratePath) {
    let file = syn::parse_file(include_str!("../src/lib.rs"))
        .expect("squallar-gpu/src/lib.rs does not parse");

    // The guard is the anonymous const that declares a trait. The backend
    // assertion next to it is also a `const _`, but declares no items.
    let mut blocks = file.items.iter().filter_map(|item| match item {
        Item::Const(c) if c.ident == "_" => match &*c.expr {
            Expr::Block(b)
                if b.block
                    .stmts
                    .iter()
                    .any(|s| matches!(s, Stmt::Item(Item::Trait(_)))) =>
            {
                Some(&b.block)
            }
            _ => None,
        },
        _ => None,
    });

    let block = blocks.next().unwrap_or_else(|| {
        panic!(
            "the wgpu single-copy guard is gone from squallar-gpu/src/lib.rs. Without it \
             a second wgpu resolves silently and this crate's backend features go to a copy \
             nothing renders through."
        )
    });
    assert!(
        blocks.next().is_none(),
        "more than one candidate guard block in lib.rs"
    );

    let mut aliases: HashMap<String, &Type> = HashMap::new();
    let mut impls: Vec<&Type> = Vec::new();
    let mut turbofish = TurbofishArgs::default();
    for stmt in &block.stmts {
        match stmt {
            Stmt::Item(Item::Type(t)) => {
                aliases.insert(t.ident.to_string(), &t.ty);
            }
            Stmt::Item(Item::Impl(i)) if i.trait_.is_some() => impls.push(&i.self_ty),
            _ => {}
        }
        turbofish.visit_stmt(stmt);
    }

    assert_eq!(
        impls.len(),
        1,
        "expected one trait impl in the wgpu guard, found {}",
        impls.len()
    );
    let args = turbofish.0;
    assert_eq!(
        args.len(),
        1,
        "expected one turbofish arg in the wgpu guard, found {}",
        args.len()
    );

    (
        crate_path(resolve(impls[0], &aliases)),
        crate_path(resolve(args[0], &aliases)),
    )
}

#[test]
fn guard_compares_two_different_crates() {
    let (impl_side, assert_side) = guard_sides();

    let rooted = u8::from(impl_side.rooted) + u8::from(assert_side.rooted);
    assert_eq!(
        rooted, 1,
        "the wgpu guard compares `{impl_side}` against `{assert_side}`, of which {rooted} are \
         `::`-rooted; exactly one must be. Bare `wgpu::` outside the guard is the `egui_wgpu::wgpu` re-export \
         at the top of the file, so a guard with no `::`-rooted side compares egui-wgpu's copy \
         against itself and passes however many wgpus are in the graph."
    );

    // Which side carries the `impl` is arbitrary; only the pair matters.
    let (ours, theirs) = if impl_side.rooted {
        (&impl_side, &assert_side)
    } else {
        (&assert_side, &impl_side)
    };

    assert_eq!(
        ours.segments.first().map(String::as_str),
        Some("wgpu"),
        "the `::`-rooted side of the wgpu guard is `{ours}`, not the `wgpu` crate"
    );
    assert!(
        theirs
            .segments
            .starts_with(&["egui_wgpu".to_owned(), "wgpu".to_owned()]),
        "the other side of the wgpu guard is `{theirs}`, which does not reach wgpu through \
         `egui_wgpu`, so it is not the copy that renders"
    );
    assert_ne!(
        ours, theirs,
        "both sides of the wgpu guard resolve to the same path"
    );
}

/// Whether a syntax tree names `::wgpu` — the direct dependency, `::`-rooted.
#[derive(Default)]
struct RootedWgpuPaths(Vec<String>);

impl RootedWgpuPaths {
    fn note(&mut self, first_segment: &syn::Ident) {
        if first_segment == "wgpu" {
            self.0.push(format!("::{first_segment}"));
        }
    }
}

impl<'ast> Visit<'ast> for RootedWgpuPaths {
    fn visit_path(&mut self, node: &'ast syn::Path) {
        if node.leading_colon.is_some()
            && let Some(first) = node.segments.first()
        {
            self.note(&first.ident);
        }
        syn::visit::visit_path(self, node);
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        if node.leading_colon.is_some() {
            match &node.tree {
                UseTree::Path(p) => self.note(&p.ident),
                UseTree::Name(n) => self.note(&n.ident),
                UseTree::Rename(r) => self.note(&r.ident),
                UseTree::Glob(_) | UseTree::Group(_) => {}
            }
        }
        syn::visit::visit_item_use(self, node);
    }
}

/// Every `.rs` file under `src/`, so a module added tomorrow is covered without
/// anyone remembering to list it. Recursive, because `#[path]` modules and real
/// subdirectories both exist in this crate's future.
fn source_files(dir: &FsPath, into: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("cannot read a directory entry").path();
        if path.is_dir() {
            source_files(&path, into);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            into.push(path);
        }
    }
}

/// `lib.rs` must be the only file in this crate that names `::wgpu`.
#[test]
fn every_wgpu_path_outside_the_guard_goes_through_egui_wgpu() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    source_files(&src, &mut files);
    files.sort();

    // A scan over nothing passes. Pin that there is something to scan, and that
    // the exemption is still earning its keep — if `lib.rs` stops naming
    // `::wgpu`, the guard has moved or gone and this test's exemption is stale.
    assert!(
        files.len() > 5,
        "only {} source files found under {}; the scan is not looking where it \
         thinks it is",
        files.len(),
        src.display()
    );

    let mut lib_rs_seen = false;
    for file in &files {
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .expect("a source file with no name");
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
        let parsed = syn::parse_file(&text)
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", file.display()));

        let mut found = RootedWgpuPaths::default();
        found.visit_file(&parsed);

        if name == "lib.rs" {
            lib_rs_seen = true;
            assert!(
                !found.0.is_empty(),
                "lib.rs no longer names `::wgpu`, so the single-copy guard has \
                 moved or gone — and this test is exempting a file that no \
                 longer holds the guard it is exempted for"
            );
            continue;
        }

        assert!(
            found.0.is_empty(),
            "{} names `::wgpu` {} time(s). Every `wgpu::` in this crate must \
             reach the copy egui-wgpu renders through, `egui_wgpu::wgpu`; the \
             `::`-rooted path is the direct dependency, which exists only to \
             *choose wgpu's features*. They resolve to the same package today, \
             so this compiles and works — until a version bump splits them, at \
             which point this file renders through a wgpu nothing configured, \
             with `guard_compares_two_different_crates` still green.",
            file.display(),
            found.0.len(),
        );
    }

    assert!(
        lib_rs_seen,
        "src/lib.rs was not scanned, so the guard's own file went unchecked"
    );
}
