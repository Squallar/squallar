#!/usr/bin/env python3
"""Reject root-absolute URL references in a staged GitHub Pages tree.

A project Pages site is served from a subpath, so an absolute path works under
`python3 -m http.server` and 404s in production: a green deploy and a blank
page. Scans every staged text asset, not just index.html -- `start_url: "/"`
breaks installability and `caches.addAll(["/"])` caches the wrong origin path,
and neither is in index.html.

Exits non-zero on any violation, and also if it was handed nothing to scan.
"""

import json
import re
import sys
from pathlib import Path

# Extensions worth reading. Binary assets (.wasm, .png) cannot carry a URL
# reference we could usefully parse.
TEXT_SUFFIXES = {
    ".html", ".htm", ".js", ".mjs", ".css", ".json", ".webmanifest", ".svg", ".txt", ".map",
}

# A root-absolute reference is `/` NOT followed by another `/` -- `//host/path`
# is protocol-relative and resolves against the origin, which is fine.
ABS = r"/(?!/)"

# (rule name, compiled pattern). Each is deliberately narrow enough to name the
# construct in the error message.
RULES = [
    # src="/x"  href='/x'  srcset = "/x"  src=/x  (quotes optional, spaces allowed)
    ("html attribute",
     re.compile(r"""\b(?:src|href|srcset|action|poster|manifest|content|data)\s*=\s*["']?""" + ABS)),
    # ES imports: `from "/x"`, `import "/x"`, `import("/x")`
    ("es import",
     re.compile(r"""\b(?:from|import)\s*\(?\s*["']""" + ABS)),
    # URL-taking calls: fetch("/x"), register("/sw.js"), importScripts("/x"),
    # new URL("/x", ...), caches.open is not a URL so it is not listed.
    ("url call",
     re.compile(r"""\b(?:fetch|register|importScripts|navigate|redirect)\s*\(\s*["']""" + ABS)),
    ("new URL",
     re.compile(r"""\bnew\s+URL\s*\(\s*["']""" + ABS)),
    # CSS url(/x) and url("/x")
    ("css url()",
     re.compile(r"""\burl\(\s*["']?""" + ABS)),
    # Service worker registration scope, and manifest-ish keys appearing in JS.
    ("scope/start_url property",
     re.compile(r"""\b(?:scope|start_url|startUrl)\s*[:=]\s*["']""" + ABS)),
]

# `caches.addAll([...])` / `addAll([...])`: every string inside the array literal
# is a URL, so a bare "/" there is a violation. Scoped to the array so that an
# unrelated `"/"` (a path separator in `.split("/")`, say) is not flagged.
ADDALL = re.compile(r"\baddAll\s*\(\s*\[(?P<body>[^\]]*)\]", re.S)
ADDALL_ABS = re.compile(r"""["']""" + ABS)

# Manifest keys whose values are URLs resolved against the manifest's location.
MANIFEST_URL_KEYS = {"start_url", "scope", "src", "url", "id"}


def scan_json_urls(path, data, violations, prefix=""):
    """Any string value under a URL-bearing key that starts with a single `/`."""
    if isinstance(data, dict):
        for key, value in data.items():
            if key in MANIFEST_URL_KEYS and isinstance(value, str):
                if value.startswith("/") and not value.startswith("//"):
                    violations.append((path, 0, f'manifest "{prefix}{key}"', f'{key}: "{value}"'))
            scan_json_urls(path, value, violations, prefix=f"{prefix}{key}.")
    elif isinstance(data, list):
        for item in data:
            scan_json_urls(path, item, violations, prefix=prefix)


def scan(root: Path):
    violations = []
    scanned = 0

    for path in sorted(root.rglob("*")):
        if not path.is_file() or path.suffix.lower() not in TEXT_SUFFIXES:
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError) as exc:
            print(f"::error::could not read {path}: {exc}")
            violations.append((path, 0, "unreadable", str(exc)))
            continue
        scanned += 1
        rel = path.relative_to(root)

        for lineno, line in enumerate(text.splitlines(), 1):
            for name, pattern in RULES:
                match = pattern.search(line)
                if match:
                    violations.append((rel, lineno, name, line.strip()[:160]))

        for match in ADDALL.finditer(text):
            if ADDALL_ABS.search(match.group("body")):
                lineno = text[: match.start()].count("\n") + 1
                violations.append((rel, lineno, "addAll() precache list",
                                   match.group(0).replace("\n", " ")[:160]))

        # Structural pass over real JSON, which catches manifest keys regardless
        # of formatting.
        if path.suffix.lower() in {".json", ".webmanifest"}:
            try:
                scan_json_urls(rel, json.loads(text), violations)
            except json.JSONDecodeError as exc:
                print(f"::error::{rel} is not valid JSON: {exc}")
                violations.append((rel, 0, "invalid JSON", str(exc)))

    return scanned, violations


def main():
    if len(sys.argv) != 2:
        print("usage: check-relative-paths.py <staged-dir>", file=sys.stderr)
        return 2
    root = Path(sys.argv[1])

    if not root.is_dir():
        print(f"::error::{root} is not a directory; nothing was staged to scan.")
        return 1

    index = root / "index.html"
    if not index.is_file():
        print(f"::error::{index} is missing. The staged tree has no entry point.")
        return 1

    scanned, violations = scan(root)

    if scanned == 0:
        print(f"::error::scanned 0 text files under {root}; the scanner is not looking at anything.")
        return 1

    if violations:
        print(f"::error::{len(violations)} root-absolute reference(s) in the staged Pages tree:")
        for path, lineno, rule, snippet in violations:
            loc = f"{path}:{lineno}" if lineno else str(path)
            print(f"  {loc}: [{rule}] {snippet}")
        print()
        print("A project Pages site is served from a subpath, so these resolve against")
        print("the wrong origin root and 404 in production. Use './' relative paths.")
        return 1

    print(f"ok: {scanned} text file(s) scanned, no root-absolute references")
    return 0


if __name__ == "__main__":
    sys.exit(main())
