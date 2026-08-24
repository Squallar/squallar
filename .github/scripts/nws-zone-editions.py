#!/usr/bin/env python3
"""Answer one question: has the NWS published a zone edition newer than the one
`tools/nws-zone-pack/src/main.rs` pins?

The pack in `squallar-web/zones.pack` is built from six AWIPS shapefile datasets,
each named `<prefix><DDMMMYY>` for the day its zone changes take effect. The
`DATASETS` table in the converter names those six, and pins a `published_records`
count for each that the converter asserts three independent ways. Both halves are
deliberately hand-maintained: the counts are a control, and a job that bumped them
to whatever the new files happened to contain would be asserting that the parse
agreed with itself.

So this script never edits anything. It reports, and `zone-pack.yaml` fails on it.

## What signal this relies on, and what it does not

**It scrapes the four NWS product pages.** Each lists the zip for every live
edition of its datasets, with the href under `/source/gis/Shapefiles/{WSOM,County}/`.
That is the only machine-readable inventory that exists:

  - The obvious alternative, listing the directory itself, does not work.
    `https://www.weather.gov/source/gis/Shapefiles/WSOM/` and `.../County/`
    both answer **HTTP 500**, not an index. Checked 2026-08-22.
  - Fetching a *named* zip does work and does 404 correctly on a name that was
    never published (checked the same day: `z_16ap26.zip` -> 200,
    `z_99xx99.zip` -> 404). That is the fallback signal, and `zone-pack.yaml`
    gets it for free by downloading with `curl --fail` -- but it only fires once
    the NWS *withdraws* the old edition, which is late. Both editions of every
    dataset are still downloadable today, so a 404 alone would have told us
    nothing for months.

The page scrape is therefore the early signal and the 404 is the backstop. If the
NWS restyles these pages the extraction below finds no zips at all, which is a
hard failure here rather than a silent pass -- see `no zips at all` below.

**Renovate does not drive this, and cannot.** A `customDatasource` needs its
versions ordered, and no Renovate versioning scheme orders `DDMMMYY`: `16ap26`
(16 April 2026) is newer than `18mr25` (18 March 2025), and every built-in
comparison -- `loose`, `semver`, `regex` -- reads the leading `18 > 16` and calls
it the other way round. A rule that resolves backwards does not warn, it silently
never fires. The month codes are two letters, so `regex` versioning has nowhere to
put them either. Hence: date parsing here, in a place that can be run and tested.
"""

import re
import sys
import urllib.error
import urllib.request
from pathlib import Path

# The NWS's two-letter AWIPS month codes. November is accepted under both
# spellings: the live pages only exhibit `fe`, `mr` and `ap` today, so `no`
# vs `nv` is the one entry not confirmed against a real filename. Accepting
# both costs nothing and turns a guess into a non-issue -- and an edition
# whose code is in neither list fails loudly below rather than being skipped.
MONTHS = {
    "ja": 1, "fe": 2, "mr": 3, "ap": 4, "my": 5, "jn": 6,
    "jl": 7, "au": 8, "se": 9, "oc": 10, "no": 11, "nv": 11, "de": 12,
}

# Which product page advertises which dataset, and which server directory the
# zip lives under. Counties are the only one not under `WSOM/`.
PRODUCT_PAGES = {
    "z_": ("https://www.weather.gov/gis/PublicZones", "WSOM"),
    "fz": ("https://www.weather.gov/gis/FireZones", "WSOM"),
    "c_": ("https://www.weather.gov/gis/Counties", "County"),
    "mz": ("https://www.weather.gov/gis/MarineZones", "WSOM"),
    "oz": ("https://www.weather.gov/gis/MarineZones", "WSOM"),
    "hz": ("https://www.weather.gov/gis/MarineZones", "WSOM"),
}

BASE = "https://www.weather.gov/source/gis/Shapefiles"

# `z_16ap26` -> ("z_", "16ap26"). The prefix is lazy so it gives back the
# characters the date needs: `z` alone would leave `_16ap26`, which does not
# start with two digits, and the engine backtracks to `z_`.
DIR_RE = re.compile(r"^(?P<prefix>[a-z_]+?)(?P<dd>\d{2})(?P<mmm>[a-z]{2})(?P<yy>\d{2})$")

# The `dir:` fields of the `DATASETS` table, in order.
DATASETS_RE = re.compile(r"const DATASETS.*?\n\];", re.S)
DIR_FIELD_RE = re.compile(r'dir:\s*"([^"]+)"')


def die(message: str) -> None:
    print(f"::error::{message}")
    sys.exit(2)


def edition_date(edition: str, prefix: str):
    """`16ap26` -> (2026, 4, 16). Sortable, which the raw string is not."""
    match = DIR_RE.match(prefix + edition)
    if not match:
        die(f"{prefix}{edition} is not <prefix><DDMMMYY>; this script cannot order it")
    month = MONTHS.get(match["mmm"])
    if month is None:
        die(
            f"{prefix}{edition} uses month code {match['mmm']!r}, which is not one this "
            f"script knows ({', '.join(sorted(MONTHS))}). Add it to MONTHS -- do not "
            f"guess which edition is newer."
        )
    return (2000 + int(match["yy"]), month, int(match["dd"]))


def pinned_datasets(repo_root: Path):
    """The six `dir:` names, read out of the converter rather than copied here."""
    source = (repo_root / "tools/nws-zone-pack/src/main.rs").read_text(encoding="utf-8")
    table = DATASETS_RE.search(source)
    if not table:
        die("could not find `const DATASETS ... ];` in tools/nws-zone-pack/src/main.rs")
    dirs = DIR_FIELD_RE.findall(table.group(0))
    if not dirs:
        die("the DATASETS table parsed as zero datasets; the parse is wrong")
    return dirs


def fetch(url: str) -> str:
    request = urllib.request.Request(url, headers={"User-Agent": "squallar-zone-pack-ci"})
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            return response.read().decode("utf-8", "replace")
    except (urllib.error.URLError, OSError) as exc:
        die(f"could not read {url}: {exc}")


def main() -> int:
    args = sys.argv[1:]
    url_file = None
    if args:
        if args[0] != "--urls" or len(args) != 2:
            print("usage: nws-zone-editions.py [--urls <out-file>]", file=sys.stderr)
            return 2
        url_file = Path(args[1])
    repo_root = Path(__file__).resolve().parents[2]

    pages: dict[str, str] = {}
    findings: list[str] = []
    urls: list[str] = []

    for name in pinned_datasets(repo_root):
        match = DIR_RE.match(name)
        if not match:
            die(f"DATASETS names {name!r}, which is not <prefix><DDMMMYY>")
        prefix = match["prefix"]
        if prefix not in PRODUCT_PAGES:
            die(
                f"DATASETS names {name!r} and no product page is registered for "
                f"prefix {prefix!r}. Add one to PRODUCT_PAGES."
            )
        page_url, directory = PRODUCT_PAGES[prefix]
        if page_url not in pages:
            pages[page_url] = fetch(page_url)

        # Every edition of this dataset the page still advertises.
        link_re = re.compile(
            rf"Shapefiles/{directory}/{re.escape(prefix)}(\d{{2}}[a-z]{{2}}\d{{2}})\.zip"
        )
        advertised = sorted(set(link_re.findall(pages[page_url])))
        if not advertised:
            # no zips at all: either the page moved or its markup changed. Never
            # a quiet pass -- a scraper that silently matches nothing is how a
            # detector stops detecting without anybody noticing.
            die(
                f"{page_url} advertises no {prefix}*.zip at all. Either the page moved "
                f"or its markup changed; this check is blind until someone fixes the "
                f"extraction. Refusing to report 'up to date'."
            )

        pinned = name[len(prefix):]
        pinned_on_page = pinned in advertised
        pinned_date = edition_date(pinned, prefix)
        newer = [e for e in advertised if edition_date(e, prefix) > pinned_date]

        print(f"{name}: page advertises {', '.join(prefix + e for e in advertised)}")
        if newer:
            # By date, not by string: `advertised` is sorted lexicographically
            # and lexicographic order is exactly what does not work here.
            newest = max(newer, key=lambda e: edition_date(e, prefix))
            findings.append(
                f"{prefix}{newest} supersedes the pinned {name} "
                f"(advertised at {page_url})"
            )
        elif not pinned_on_page:
            findings.append(
                f"{name} is no longer advertised at {page_url}; it has been withdrawn "
                f"and the pin is stale"
            )
        urls.append(f"{name} {BASE}/{directory}/{name}.zip")

    if findings:
        print("::error::the NWS has published a zone edition newer than the pinned one.")
        for finding in findings:
            print(f"::error::  {finding}")
        print("::error::")
        print("::error::This job will not re-pin it. Both halves of a DATASETS row are")
        print("::error::hand-maintained on purpose: `dir` names the edition, and")
        print("::error::`published_records` is the count weather.gov prints beside the")
        print("::error::download, asserted against the .shx index, the .shp record loop")
        print("::error::and the .dbf row count. A bot that copied the new counts out of")
        print("::error::the new files would be asserting the parser agrees with itself.")
        print("::error::")
        print("::error::To fix: edit tools/nws-zone-pack/src/main.rs -- update `dir` AND")
        print("::error::`published_records` for each row above from the product page,")
        print("::error::then re-run this workflow. It will rebuild the pack and commit it.")
        return 2

    if url_file is not None:
        url_file.write_text("\n".join(urls) + "\n", encoding="utf-8")
        print(f"::group::download URLs -> {url_file}")
        for line in urls:
            print(f"  {line}")
        print("::endgroup::")

    print(f"ok: all {len(urls)} pinned editions are the newest the NWS advertises")
    return 0


if __name__ == "__main__":
    sys.exit(main())
