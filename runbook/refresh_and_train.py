#!/usr/bin/env python3
"""One command that refreshes the metagame and retrains the leaf evaluator.

Run it from the repository root:

    python runbook/refresh_and_train.py

`runbook/REFRESH_AND_TRAIN.md` explains each stage and how to read the report.

The script runs six stages in order. Each one can be skipped, so a stopped run
resumes without repeating the work that already finished.

1. pastes    Read a VGCPastes export, fetch every Pokepaste, write teamsheets.
2. meta      Refresh the championsbattledata.com usage cache.
3. build     Build `train_eval` in the release profile.
4. reset     Restore the three weight files to their committed values.
5. calibrate Measure the label cost and size the training run from it.
6. train     Collect the corpus, label it, fit the weights, write the report.

The standard library is the only dependency, which matches
`meta_scraper/update_meta.py`.

Data comes from VGCPastes and from Pokemon Champions Battle Data
(https://championsbattledata.com/). Credit both wherever this data is used.
"""

import argparse
import concurrent.futures
import csv
import io
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone

# ── Layout ──────────────────────────────────────────────────────────────────

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
POKE_RUST = os.path.join(ROOT, "poke_rust")
TEAMSHEET_DIR = os.path.join(ROOT, "teamsheets", "vgcpastes")
EXPORT_DIR = os.path.join(ROOT, "teamsheets", "vgcpastes_exports")
META_SCRAPER = os.path.join(ROOT, "meta_scraper", "update_meta.py")
META_ROOT = os.path.join(ROOT, "meta_scraper", "data")
LOG_DIR = os.path.join(ROOT, "runbook", "logs")
CALIBRATION = os.path.join(LOG_DIR, "calibration.json")

# The three files that a training run writes. `TRAINING.md` requires all three
# to hold their committed values before a run starts, because the fit compares
# itself against them.
WEIGHT_FILES = [
    "weights/eval_v1.json",
    "weights/eval_mlp_v1.json",
    "weights/policy_v1.json",
]

BINARY = os.path.join(POKE_RUST, "target", "release", "train_eval")
if os.name == "nt":
    BINARY += ".exe"

# ── Pokepaste fetching ──────────────────────────────────────────────────────

USER_AGENT = "PokeRust-runbook/1.0 (personal/non-commercial use)"
PASTE_WORKERS = 6
PASTE_DELAY = 0.12  # seconds per request; the site documents no rate limit
PASTE_RETRIES = 3

# A Pokepaste URL, with or without a trailing slash or an existing /raw suffix.
PASTE_URL = re.compile(r"https?://pokepast\.es/([0-9a-f]+)", re.I)


def log(message):
    stamp = datetime.now().strftime("%H:%M:%S")
    print("[%s] %s" % (stamp, message), flush=True)


def paste_links(export_path):
    """Every distinct Pokepaste id in one VGCPastes export.

    The export is a spreadsheet dump, so the header row is not row zero and the
    link column moves between exports. Scanning every cell for the URL pattern
    survives both, and it also catches a link that sits in a note column.
    """
    ids = []
    seen = set()
    with io.open(export_path, encoding="utf-8", newline="") as handle:
        for row in csv.reader(handle):
            for cell in row:
                match = PASTE_URL.search(cell or "")
                if match:
                    paste_id = match.group(1).lower()
                    if paste_id not in seen:
                        seen.add(paste_id)
                        ids.append(paste_id)
    return ids


def fetch_paste(paste_id):
    """The raw teamsheet text of one paste, or None when it cannot be read.

    `/raw` returns the paste in teamsheet format, which is the format that
    `parse_team_sheet_str` reads. No conversion step is needed.
    """
    url = "https://pokepast.es/%s/raw" % paste_id
    last = None
    for attempt in range(PASTE_RETRIES):
        try:
            request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
            with urllib.request.urlopen(request, timeout=20) as response:
                return response.read().decode("utf-8", errors="replace")
        except urllib.error.HTTPError as error:
            if error.code in (404, 410):
                return None  # the paste is gone; not worth a retry
            last = error
        except (urllib.error.URLError, OSError) as error:
            last = error
        time.sleep(0.5 * (attempt + 1))
    log("  ! %s: %s" % (paste_id, last))
    return None


def looks_like_a_team(text):
    """A cheap check that the body is a teamsheet and not an error page.

    The Rust parser drops a block it cannot read, so a page of HTML would parse
    into an empty roster and waste a slot in the pool. Counting the ability and
    move lines rejects that case here instead.
    """
    if not text or "<html" in text[:200].lower():
        return False
    return text.count("Ability:") >= 3 and text.count("\n- ") >= 8


def stage_pastes(args):
    exports = args.export
    if not exports:
        if not os.path.isdir(EXPORT_DIR):
            sys.exit("no export directory at %s; pass --export" % EXPORT_DIR)
        exports = [
            os.path.join(EXPORT_DIR, name)
            for name in sorted(os.listdir(EXPORT_DIR))
            if name.lower().endswith(".csv")
        ]
    if not exports:
        sys.exit("no .csv export found; pass --export")

    ids = []
    seen = set()
    for export in exports:
        found = paste_links(export)
        log("%s: %d paste link(s)" % (os.path.basename(export), len(found)))
        for paste_id in found:
            if paste_id not in seen:
                seen.add(paste_id)
                ids.append(paste_id)

    os.makedirs(TEAMSHEET_DIR, exist_ok=True)
    pending = [
        paste_id
        for paste_id in ids
        if not os.path.exists(os.path.join(TEAMSHEET_DIR, "%s.txt" % paste_id))
    ]
    have = len(ids) - len(pending)
    log("%d unique paste(s); %d already on disk, %d to fetch" % (len(ids), have, len(pending)))
    if args.max_teams and len(pending) > args.max_teams:
        pending = pending[: args.max_teams]
        log("  limited to %d by --max-teams" % args.max_teams)

    written = 0
    rejected = 0

    def worker(paste_id):
        time.sleep(PASTE_DELAY)
        return paste_id, fetch_paste(paste_id)

    if pending:
        with concurrent.futures.ThreadPoolExecutor(max_workers=PASTE_WORKERS) as pool:
            futures = [pool.submit(worker, paste_id) for paste_id in pending]
            for done, future in enumerate(concurrent.futures.as_completed(futures), 1):
                paste_id, text = future.result()
                if looks_like_a_team(text):
                    path = os.path.join(TEAMSHEET_DIR, "%s.txt" % paste_id)
                    with io.open(path, "w", encoding="utf-8", newline="\n") as handle:
                        handle.write(text)
                    written += 1
                else:
                    rejected += 1
                if done % 50 == 0 or done == len(pending):
                    log("  %d/%d fetched" % (done, len(pending)))

    total = len([n for n in os.listdir(TEAMSHEET_DIR) if n.endswith(".txt")])
    log("teamsheets: %d written, %d rejected, %d on disk" % (written, rejected, total))
    if total == 0:
        sys.exit("no teamsheet was written; the training stage would have no teams")
    return total


# ── The other stages ────────────────────────────────────────────────────────


def run(command, cwd=None, log_path=None):
    """Runs a command, echoing its output and optionally teeing it to a file."""
    log("$ " + " ".join(command))
    lines = []
    process = subprocess.Popen(
        command,
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        encoding="utf-8",
        errors="replace",
        bufsize=1,
    )
    for line in process.stdout:
        sys.stdout.write(line)
        sys.stdout.flush()
        lines.append(line)
    process.wait()
    if log_path:
        with io.open(log_path, "w", encoding="utf-8", newline="\n") as handle:
            handle.writelines(lines)
    return process.returncode, "".join(lines)


def stage_meta(args):
    command = [sys.executable, META_SCRAPER, "--format", args.meta_format]
    code, _ = run(command, cwd=ROOT)
    if code != 0:
        sys.exit("the usage-cache refresh failed with exit code %d" % code)
    index = os.path.join(META_ROOT, "index.json")
    if os.path.exists(index):
        with io.open(index, encoding="utf-8") as handle:
            season = json.load(handle).get("season")
        log("usage cache season: %r" % season)


def stage_build(args):
    code, _ = run(
        ["cargo", "build", "--release", "--bin", "train_eval"],
        cwd=POKE_RUST,
    )
    if code != 0:
        sys.exit("the release build failed with exit code %d" % code)


def read_feature_frame():
    """The feature names, the hand weights, and the seed scale, from `eval.rs`.

    Reading them from the source keeps this script correct after a feature
    addition. There is no second copy to update.
    """
    path = os.path.join(POKE_RUST, "src", "solver", "eval.rs")
    with io.open(path, encoding="utf-8") as handle:
        source = handle.read()

    names = re.search(
        r"pub const FEATURE_NAMES: \[&str; FEATURE_COUNT\] = \[(.*?)\];", source, re.S
    )
    hand = re.search(r"pub const HAND_WEIGHTS: Features = \[(.*?)\];", source, re.S)
    scale = re.search(r"const MLP_SEED_SCALE: f64 = ([0-9.]+);", source)
    if not (names and hand and scale):
        sys.exit("could not read the feature frame from src/solver/eval.rs")

    names = re.findall(r'"([a-z_]+)"', names.group(1))
    weights = [float(value) for value in re.findall(r"^\s*(-?[0-9.]+),", hand.group(1), re.M)]
    if len(names) != len(weights):
        sys.exit(
            "FEATURE_NAMES has %d entries but HAND_WEIGHTS has %d"
            % (len(names), len(weights))
        )
    return names, weights, float(scale.group(1))


def reseed_network(names, weights, scale):
    """Writes the hand-seeded network at the current feature width.

    `MLP_HIDDEN` equals `FEATURE_COUNT`, so a feature addition changes the
    hidden-layer width as well as the column count. A record from the earlier
    width cannot be reshaped, and `MlpRecord::to_network` correctly refuses it.
    The linear file does not have this problem: `resolve` fills one name at a
    time, so a name it does not find keeps its hand-set value.

    This mirrors `Mlp::seed` in `src/solver/eval.rs`.
    """
    count = len(names)
    hidden = [[0.0] * count for _ in range(count)]
    output = [0.0] * count
    for unit in range(count):
        feature = unit % count
        copies = (count - 1 - feature) // count + 1
        hidden[unit][feature] = scale
        output[unit] = weights[feature] / (scale * copies)

    path = os.path.join(POKE_RUST, "weights", "eval_mlp_v1.json")
    with io.open(path, "w", encoding="utf-8", newline="\n") as handle:
        json.dump({"features": names, "hidden": hidden, "output": output}, handle, indent=2)
        handle.write("\n")


def stage_reset(args):
    """Restores the three weight files, then repairs a stale feature width.

    The labels come from `solve`, and `solve` scores its own horizon with the
    committed weights. Training is therefore a fixed-point step, not a run that
    converges on its own. Starting from a previous run's output would compare
    the new fit against that output rather than against the baseline, and the
    accept rule would stop meaning anything.

    A feature addition makes the committed network file the wrong shape, so the
    restore has to be followed by a width check.
    """
    # The restore discards whatever the files hold now. An accepted run that
    # nobody committed yet lives exactly there, and four hours of labeling would
    # go with it. Ask first.
    dirty = subprocess.run(
        ["git", "status", "--porcelain", "--"] + WEIGHT_FILES,
        cwd=POKE_RUST,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if dirty and not args.force_reset:
        log("! the weight files hold uncommitted changes:")
        for line in dirty.splitlines():
            log("!   " + line.strip())
        log("! `reset` would discard them, and an accepted run is not recoverable.")
        log("! Commit them first, or pass --force-reset to discard them.")
        sys.exit("stopped before the reset")

    code, _ = run(["git", "checkout", "--"] + WEIGHT_FILES, cwd=POKE_RUST)
    if code != 0:
        log("! could not restore the weight files; check `git status` in poke_rust/")

    names, weights, scale = read_feature_frame()
    network = os.path.join(POKE_RUST, "weights", "eval_mlp_v1.json")
    stored = []
    if os.path.exists(network):
        try:
            with io.open(network, encoding="utf-8") as handle:
                stored = json.load(handle).get("features", [])
        except (ValueError, OSError):
            stored = []
    if stored != names:
        log("network file holds %d feature(s), the frame holds %d; reseeding"
            % (len(stored), len(names)))
        reseed_network(names, weights, scale)
    log("feature frame: %d features (%s)" % (len(names), ", ".join(names)))


def train_command(args, positions, deadline, time_budget):
    """The training invocation, shared by the calibrate and the train stages."""
    command = [
        BINARY,
        "--teamsheet-dir", os.path.relpath(TEAMSHEET_DIR, POKE_RUST),
        "--teamsheet-mix", str(args.teamsheet_mix),
        "--meta-root", os.path.relpath(META_ROOT, POKE_RUST),
        "--label-depth", str(args.label_depth),
        "--min-label-depth", "1",
        "--label-chance", args.label_chance,
        "--label-max-actions", str(args.label_max_actions),
        "--iterative-deepening",
        "--workers", str(args.workers),
        "--seed", str(args.seed),
        "--positions", str(positions),
    ]
    if deadline:
        command += ["--label-deadline", "%.1f" % deadline]
    if time_budget:
        command += ["--time-budget", "%.0f" % time_budget]
    return command


def stage_calibrate(args):
    """Measures the label cost and returns (labels per second, max label cost).

    Use at least three times as many positions as workers. A sample that fits in
    one wave measures the slowest label rather than the rate.
    """
    positions = max(args.calibrate_positions, args.workers * 3)
    command = train_command(args, positions, deadline=None, time_budget=None)
    command += ["--calibrate", "--calibrate-positions", str(positions)]
    code, output = run(command, cwd=POKE_RUST)
    if code != 0:
        sys.exit("calibration failed with exit code %d" % code)

    rate = re.search(r"([0-9.]+) labels per second", output)
    worst = re.search(r"max ([0-9.]+) s", output)
    if not rate or not worst:
        sys.exit("could not read the calibration report; run the stage by hand")

    rate, worst = float(rate.group(1)), float(worst.group(1))
    # Written so that a later `--only train` sizes itself from a real
    # measurement rather than from a default. Label cost depends on the depth,
    # the chance mode, the action cap, and the worker count, so the record holds
    # them and a run that changed any of them ignores it.
    os.makedirs(LOG_DIR, exist_ok=True)
    with io.open(CALIBRATION, "w", encoding="utf-8", newline="\n") as handle:
        json.dump(
            {
                "rate": rate,
                "worst": worst,
                "measured": datetime.now(timezone.utc).isoformat(),
                "settings": calibration_key(args),
            },
            handle,
            indent=2,
        )
    return rate, worst


def calibration_key(args):
    """The settings that change the cost of one label."""
    return {
        "label_depth": args.label_depth,
        "label_chance": args.label_chance,
        "label_max_actions": args.label_max_actions,
        "workers": args.workers,
    }


def stored_calibration(args):
    """A previous measurement, when it was taken at these settings."""
    if not os.path.exists(CALIBRATION):
        return None
    try:
        with io.open(CALIBRATION, encoding="utf-8") as handle:
            record = json.load(handle)
    except (ValueError, OSError):
        return None
    if record.get("settings") != calibration_key(args):
        log("stored calibration used other settings; ignoring it")
        return None
    return float(record["rate"]), float(record["worst"])


def stage_train(args, rate, worst):
    """Runs the labeling and fitting stage, sized from the calibration."""
    seconds = args.hours * 3600.0
    # 15 percent of headroom, so the corpus does not run dry before the clock
    # stops the stage. A corpus that runs out ends the run early.
    positions = int(rate * seconds * 1.15) + args.workers
    # Above the slowest label seen, so a normal label is never cut short. With
    # iterative deepening a cut label still returns its last complete pass.
    deadline = max(worst * 2.0, 30.0)

    log("sizing: %.2f labels/s over %.1f h -> %d positions, %.0f s label deadline"
        % (rate, args.hours, positions, deadline))

    os.makedirs(LOG_DIR, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%SZ")
    log_path = os.path.join(LOG_DIR, "train-%s.log" % stamp)

    command = train_command(args, positions, deadline, seconds)
    if args.dry_run:
        command.append("--dry-run")
    code, output = run(command, cwd=POKE_RUST, log_path=log_path)
    log("training log: %s" % log_path)
    if code != 0:
        sys.exit("the training stage failed with exit code %d" % code)
    return output, log_path


def report(output, log_path):
    """Prints the accept decision that `TRAINING.md` defines.

    Keep a run only when the fitted weights beat the hand-set weights on the
    held-out split. A higher held-out error means the step overshot; restore the
    weight files and discard the run.
    """
    errors = {}
    for line in output.splitlines():
        match = re.match(r"\s*value\s+(\w+):.*held-out loss [0-9.]+ mae ([0-9.]+)", line)
        if match:
            errors[match.group(1)] = float(match.group(2))

    print()
    print("=" * 68)
    if "hand" in errors and "fitted" in errors:
        hand, fitted = errors["hand"], errors["fitted"]
        print("held-out mean absolute error:  hand %.4f   fitted %.4f" % (hand, fitted))
        if fitted < hand:
            print("ACCEPT: the fit beat the hand weights by %.4f." % (hand - fitted))
            print("Commit these files from poke_rust/:")
            for path in WEIGHT_FILES:
                print("  %s" % path)
            print("  benches/RESULTS.md   (record the settings of this run)")
        else:
            print("DISCARD: the fit lost by %.4f." % (fitted - hand))
            print("Restore the weights and lower --learning-rate or raise --l2:")
            print("  cd poke_rust && git checkout -- " + " ".join(WEIGHT_FILES))
    else:
        print("could not read the accept lines; read the log at:")
        print("  %s" % log_path)
    print("=" * 68)


# ── Entry point ─────────────────────────────────────────────────────────────

# `reset` runs before `build` on purpose. `src/solver/eval.rs` embeds the three
# weight files with `include_str!`, so their contents are fixed at compile time.
# A build that ran before the reset would carry the previous weights, and the
# labels would then teach against the wrong baseline.
STAGES = ["pastes", "meta", "reset", "build", "calibrate", "train"]


def parse_args():
    parser = argparse.ArgumentParser(
        description="Refresh the metagame and retrain the leaf evaluator.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="runbook/REFRESH_AND_TRAIN.md explains every stage.",
    )
    parser.add_argument(
        "--export", action="append",
        help="A VGCPastes export .csv (repeatable). Default: every .csv in "
             "teamsheets/vgcpastes_exports/.",
    )
    parser.add_argument(
        "--hours", type=float, default=4.0,
        help="Wall-clock hours for the labeling stage (default: 4).",
    )
    parser.add_argument(
        "--workers", type=int, default=max(1, (os.cpu_count() or 4) - 2),
        help="Labeling threads. Default: two below the core count.",
    )
    parser.add_argument("--seed", type=int, default=1, help="Corpus and label seed.")
    parser.add_argument(
        "--skip", action="append", choices=STAGES, default=[],
        help="Skip one stage (repeatable).",
    )
    parser.add_argument(
        "--only", action="append", choices=STAGES,
        help="Run only these stages (repeatable).",
    )
    parser.add_argument(
        "--max-teams", type=int, default=0,
        help="Fetch at most this many new pastes. Zero means every paste.",
    )
    parser.add_argument(
        "--teamsheet-mix", type=float, default=0.8,
        help="Fraction of matchups that use an archived team (default: 0.8).",
    )
    parser.add_argument("--label-depth", type=int, default=2)
    parser.add_argument(
        "--label-chance", default="top1", choices=["enumerate", "top4", "top1"],
        help="Successors that a label keeps at each chance node.",
    )
    parser.add_argument("--label-max-actions", type=int, default=24)
    parser.add_argument("--calibrate-positions", type=int, default=60)
    parser.add_argument("--meta-format", default="Doubles", choices=["Doubles", "Singles"])
    parser.add_argument(
        "--dry-run", action="store_true",
        help="Report the fit without writing any weight file.",
    )
    parser.add_argument(
        "--force-reset", action="store_true",
        help="Let the reset stage discard uncommitted weight files.",
    )
    return parser.parse_args()


def main():
    args = parse_args()
    wanted = set(args.only) if args.only else set(STAGES)
    wanted -= set(args.skip)

    # Made before any stage runs, so a caller can redirect this script's own
    # output into it on the same command line.
    os.makedirs(LOG_DIR, exist_ok=True)

    started = time.time()
    log("PokeRust metagame refresh and evaluator retraining")
    log("stages: %s" % ", ".join(stage for stage in STAGES if stage in wanted))

    if "pastes" in wanted:
        stage_pastes(args)
    if "meta" in wanted:
        stage_meta(args)
    if "reset" in wanted:
        stage_reset(args)
    if "build" in wanted:
        stage_build(args)
    if not os.path.exists(BINARY):
        sys.exit("no train_eval binary at %s; run the build stage" % BINARY)

    rate, worst = None, None
    if "calibrate" in wanted:
        rate, worst = stage_calibrate(args)
        log("calibration: %.2f labels/s, slowest label %.1f s" % (rate, worst))
    if "train" in wanted:
        if rate is None:
            # A skipped calibration reads the last measurement at these
            # settings. Without one the run falls back to a figure from a
            # depth-2 doubles label on twenty workers, which is only a guess.
            stored = stored_calibration(args)
            if stored:
                rate, worst = stored
                log("using the stored calibration: %.2f labels/s" % rate)
            else:
                rate, worst = 0.5, 90.0
                log("no calibration on file; assuming %.2f labels/s" % rate)
        output, log_path = stage_train(args, rate, worst)
        report(output, log_path)

    log("finished in %.1f minutes" % ((time.time() - started) / 60.0))


if __name__ == "__main__":
    main()
