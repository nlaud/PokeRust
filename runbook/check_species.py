#!/usr/bin/env python3
"""Lists every usage-cache species name that the dex cannot resolve.

`MetaDex::load` treats an unknown species name as an error and stops at the
first one. A season refresh can add several new names at once, so a rebuild
between each one is slow. This reports all of them in one pass.

    python runbook/check_species.py
    python runbook/check_species.py --format Singles

Add each reported name to `SPECIES_OVERRIDES` in
`poke_rust/src/meta/names.rs`, then rebuild.

This mirrors `Species::from_str` and `resolve_species`. It is a diagnostic, not
a second source of truth: the Rust code stays the authority.
"""

import argparse
import glob
import io
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SPECIES_RS = os.path.join(ROOT, "poke_rust", "src", "data", "species.rs")
NAMES_RS = os.path.join(ROOT, "poke_rust", "src", "meta", "names.rs")
META_ROOT = os.path.join(ROOT, "meta_scraper", "data")


def normalize(raw):
    """The key that `Species::from_str` builds: alphanumeric, lowercase."""
    return "".join(c.lower() for c in raw if c.isalnum())


def known_keys():
    """Every key that the dex or the override table resolves."""
    with io.open(SPECIES_RS, encoding="utf-8") as handle:
        dex = set(re.findall(r'^\s*"([a-z0-9]+)" => Species::', handle.read(), re.M))
    with io.open(NAMES_RS, encoding="utf-8") as handle:
        overrides = set(re.findall(r'\("([a-z0-9]+)", Species::', handle.read()))
    return dex, overrides


def season_dir(fmt):
    """The active season directory, from `index.json`.

    An update can create a new season directory and leave the old ones in place,
    so a fixed name goes stale. `MetaDex::load` reads the index for the same
    reason.
    """
    index = os.path.join(META_ROOT, "index.json")
    if os.path.exists(index):
        with io.open(index, encoding="utf-8") as handle:
            season = json.load(handle).get("season")
        candidate = os.path.join(META_ROOT, season or "", fmt)
        if os.path.isdir(candidate):
            return candidate
    # No index, so fall back to the last directory in lexical order.
    seasons = sorted(
        name for name in os.listdir(META_ROOT)
        if os.path.isdir(os.path.join(META_ROOT, name))
    )
    for name in reversed(seasons):
        candidate = os.path.join(META_ROOT, name, fmt)
        if os.path.isdir(candidate):
            return candidate
    sys.exit("no usage cache under %s; run meta_scraper/update_meta.py" % META_ROOT)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--format", default="Doubles", choices=["Doubles", "Singles"])
    args = parser.parse_args()

    directory = season_dir(args.format)
    dex, overrides = known_keys()

    # Both the file subject and every teammate row name a species, and either
    # one can be the name that stops the load.
    names = {}
    for path in glob.glob(os.path.join(directory, "*.json")):
        if os.path.basename(path) == "_summary.json":
            continue
        with io.open(path, encoding="utf-8") as handle:
            data = json.load(handle)
        for name in [data.get("pokemon")] + [
            row.get("name")
            for row in data.get("rows", [])
            if row.get("category") == "teammate"
        ]:
            if name:
                names.setdefault(name, os.path.basename(path))

    unresolved = sorted(
        (name, source)
        for name, source in names.items()
        if normalize(name) not in dex and normalize(name) not in overrides
    )

    print("%s: %d distinct species name(s)" % (directory, len(names)))
    if not unresolved:
        print("every name resolves")
        return

    print("%d name(s) do not resolve:" % len(unresolved))
    for name, source in unresolved:
        print("  %-28s (first seen in %s)" % (repr(name), source))
    print()
    print("Add each one to SPECIES_OVERRIDES in poke_rust/src/meta/names.rs:")
    for name, _ in unresolved:
        print('    ("%s", Species::TODO),' % normalize(name))
    sys.exit(1)


if __name__ == "__main__":
    main()
