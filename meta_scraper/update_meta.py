"""Updater for the local championsbattledata.com meta-stats cache.

Fetches the site's documented, no-auth JSON API (see README.md in this folder)
and writes the results under meta_scraper/data/ (gitignored -- this is a
regenerable cache, never committed; re-run this script to refresh it).

Data is sourced from "Pokemon Champions Battle Data" (https://championsbattledata.com/).
Per that site's license, always credit it and link to it wherever this data is used.

Usage (the "one-liner"):
    python meta_scraper/update_meta.py

Narrower re-runs while debugging:
    python meta_scraper/update_meta.py --format Doubles --pokemon garchomp --pokemon tyranitar
"""

import argparse
import concurrent.futures
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

BASE_URL = "https://championsbattledata.com"
DEFAULT_SEASON = "Current"  # the site's alias for "whichever season is live now"
FORMATS = ("Doubles", "Singles")
DATA_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data")
USER_AGENT = "PokeRust-meta-scraper/1.0 (personal/non-commercial use; see README.md)"
REQUEST_DELAY = 0.15  # seconds, per request -- no documented rate limit, be polite anyway
MAX_WORKERS = 6
MAX_RETRIES = 3


def fetch_json(url, retries=MAX_RETRIES):
    """GET `url` and parse the JSON body. Retries with backoff on transient
    network errors; a 404 is NOT retried -- it means this Pokemon genuinely has
    no data for the requested format, which callers handle by skipping."""
    last_err = None
    for attempt in range(retries):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
            with urllib.request.urlopen(req, timeout=15) as resp:
                return json.loads(resp.read().decode("utf-8"))
        except urllib.error.HTTPError as e:
            if e.code == 404:
                raise
            last_err = e
        except (urllib.error.URLError, OSError) as e:
            last_err = e
        time.sleep(0.5 * (attempt + 1))
    raise last_err


def fetch_index():
    return fetch_json(f"{BASE_URL}/api/index")


def fetch_battle(fmt, slug, season):
    url = f"{BASE_URL}/api/battle/{fmt}/{slug}?season={urllib.parse.quote(season)}"
    return fetch_json(url)


def write_json(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2, ensure_ascii=False)


def parse_args():
    p = argparse.ArgumentParser(description="Update the local championsbattledata.com meta-stats cache.")
    p.add_argument(
        "--season", default=DEFAULT_SEASON,
        help='Season request parameter (default: "Current", the site\'s "latest" alias).',
    )
    p.add_argument(
        "--format", dest="formats", action="append", choices=FORMATS,
        help="Restrict to one format (repeatable). Default: both Doubles and Singles.",
    )
    p.add_argument(
        "--pokemon", dest="pokemon", action="append",
        help="Restrict to one Pokemon by slug or name (repeatable). Default: the entire roster.",
    )
    return p.parse_args()


def main():
    args = parse_args()
    formats = args.formats or list(FORMATS)

    print(f"Fetching roster index from {BASE_URL}/api/index ...")
    index = fetch_index()
    roster = index["pokemon"]
    if args.pokemon:
        wanted = {p.lower() for p in args.pokemon}
        roster = [p for p in roster if p["slug"] in wanted or p["name"].lower() in wanted]
    print(f"Roster: {len(roster)} Pokemon, formats: {list(formats)}, season param: {args.season!r}")

    # Build the (pokemon, format) work list from each entry's own `battleDataCsvs`
    # rather than blindly trying every combination -- the index already states
    # exactly which combos have real data, avoiding wasted 404 requests.
    jobs = []
    for mon in roster:
        available = {c["format"] for c in mon.get("battleDataCsvs", []) if c["season"] == args.season}
        for fmt in formats:
            if fmt in available:
                jobs.append((mon, fmt))
    print(f"{len(jobs)} (Pokemon, format) combinations to fetch...")

    def worker(job):
        mon, fmt = job
        time.sleep(REQUEST_DELAY)
        try:
            return mon, fmt, fetch_battle(fmt, mon["slug"], args.season)
        except urllib.error.HTTPError as e:
            if e.code == 404:
                return mon, fmt, None
            print(f"  ! {mon['name']} ({fmt}): HTTP {e.code}", file=sys.stderr)
            return mon, fmt, None
        except Exception as e:  # noqa: BLE001 -- log and continue; one bad
            # Pokemon must never abort an otherwise-complete sweep.
            print(f"  ! {mon['name']} ({fmt}): {e}", file=sys.stderr)
            return mon, fmt, None

    results = []
    skipped = 0
    resolved_season = None
    with concurrent.futures.ThreadPoolExecutor(max_workers=MAX_WORKERS) as pool:
        futures = [pool.submit(worker, job) for job in jobs]
        for i, fut in enumerate(concurrent.futures.as_completed(futures), start=1):
            mon, fmt, data = fut.result()
            if data is None:
                skipped += 1
            else:
                # The response echoes the ACTUAL resolved season name (e.g.
                # "Season M-3") even when the request used the "Current" alias --
                # use that for on-disk folder naming, not the literal request param.
                if resolved_season is None:
                    resolved_season = data.get("season", args.season)
                results.append((mon, fmt, data))
            if i % 25 == 0 or i == len(jobs):
                print(f"  {i}/{len(jobs)} done ({skipped} skipped so far)")

    season_label = resolved_season or args.season
    print(f"Resolved season label: {season_label!r}")

    # Write each response verbatim, mirroring the site's own path convention:
    # data/<season>/<format>/<pokemon-slug>.json. No reshaping needed -- the API
    # payload already has moves/items/teammates/abilities/spreads all in one place.
    #
    # Also aggregate a per-format teammate-appearance count across the whole
    # sweep. The API has no site-wide "usage rank" field for a Pokemon itself
    # (confirmed against the live API, not just docs) -- this is the closest
    # available proxy for "how central is this Pokemon to the metagame," and is
    # documented as an approximation in README.md.
    teammate_counts = {fmt: {} for fmt in formats}
    present = {fmt: [] for fmt in formats}

    for mon, fmt, data in results:
        out_path = os.path.join(DATA_DIR, season_label, fmt, f"{mon['slug']}.json")
        write_json(out_path, data)
        present[fmt].append(mon["name"])
        for row in data.get("rows", []):
            if row.get("category") == "teammate" and row.get("name"):
                teammate_counts[fmt][row["name"]] = teammate_counts[fmt].get(row["name"], 0) + 1

    for fmt in formats:
        summary = {
            "format": fmt,
            "season": season_label,
            "pokemonWithData": sorted(present[fmt]),
            "teammateAppearanceCounts": dict(
                sorted(teammate_counts[fmt].items(), key=lambda kv: -kv[1])
            ),
        }
        write_json(os.path.join(DATA_DIR, season_label, fmt, "_summary.json"), summary)

    write_json(
        os.path.join(DATA_DIR, "index.json"),
        {
            "generatedAt": index.get("generatedAt"),
            "season": season_label,
            "formats": list(formats),
            "pokemonCount": len(roster),
            "source": f"{BASE_URL}/api/index",
            "attribution": "Data from Pokemon Champions Battle Data (https://championsbattledata.com/)",
        },
    )

    print(f"Done. {len(results)} files written, {skipped} combination(s) skipped (no data).")


if __name__ == "__main__":
    main()
