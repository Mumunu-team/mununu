#!/usr/bin/env python3
"""N0.1 (2026-11-03) — the executable ledger: re-run only the rows whose cone changed.

`measurements/ledger/verdicts.csv` records, per (design, row): the cone hash of what decides
the row, the measured verdict, the decider, the seconds. `make ledger-check` (this script) asks
the engine for every row's cone hash (a lift, no engine: `MUNUNU_VERIFY_AUTO_ONLY_ROWS=-` with
`MUNUNU_VERIFY_AUTO_ROW_FACTS`), re-runs the rows that are NEW, whose hash CHANGED or whose
recorded decider was RETIRED (`--retired spacer`), under a budget of 3x the recorded time (10 s
floor, 60 s cap), and compares:

  CONTRADICTION  a definite verdict became the other definite verdict   -> exit 1
  REGRESSION     a definite verdict became unknown                       -> exit 1
  IMPROVED       an unknown row decided                                  -> recorded
  SAME / NEW     as expected                                             -> recorded

Rows that were unknown are not re-run by default (breadth is `--all`, a separate, longer run).
`--write` updates the CSV in place (new, changed, improved and retired rows; rows the design no
longer has are dropped). Without it the run is a dry comparison.

Usage (in the mununu-sva image, repo root):
  scripts/ledger_check.py [--write] [--all] [--retired ENGINE]... [--only DESIGN]...
                          [--mununu PATH] [--ledger DIR]
  MUNUNU_BIN names the binary when --mununu is not given; else target/release/mununu or
  $CARGO_TARGET_DIR/release/mununu (`make build`).
Exit: 0 clean, 1 a contradiction or regression, 2 a setup error (no binary, a design that
produced no facts).
"""
import argparse
import csv
import datetime as dt
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

COLUMNS = [
    "design", "row", "property", "label", "cone_hash", "outcome", "decided_by",
    "bottom_reason", "elapsed_ms", "measured_at", "engine_rev",
]
TIMING = re.compile(r"\[mununu-timing\] property=(\S+) outcome=\S+ elapsed_ms=(\d+) phase=")
DEFINITE = {"holds", "violated"}
BUDGET_FLOOR_MS, BUDGET_CAP_MS = 10_000, 60_000


def find_mununu(explicit):
    for cand in [explicit, os.environ.get("MUNUNU_BIN")]:
        if cand:
            p = Path(cand)
            if p.is_file():
                return p.resolve()
            sys.exit(f"ledger-check: mununu binary not found at {cand}")
    for base in [os.environ.get("CARGO_TARGET_DIR"), "target"]:
        if base and (Path(base) / "release" / "mununu").is_file():
            return (Path(base) / "release" / "mununu").resolve()
    sys.exit("ledger-check: no mununu binary (make build, or --mununu / MUNUNU_BIN)")


def read_designs(path):
    rows = []
    for line in path.read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) != 3:
            sys.exit(f"ledger-check: {path}: expected design<TAB>cwd<TAB>command: {line!r}")
        rows.append(tuple(p.strip() for p in parts))
    return rows


def read_ledger(path):
    ledger = {}
    if not path.exists():
        return ledger
    with path.open(newline="") as f:
        for r in csv.DictReader(f):
            ledger[(r["design"], r["row"])] = r
    return ledger


def write_ledger(path, rows):
    rows = sorted(rows.values(), key=lambda r: (r["design"], r["row"]))
    with path.open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=COLUMNS, lineterminator="\n")
        w.writeheader()
        for r in rows:
            w.writerow({c: r.get(c, "") for c in COLUMNS})


def run_design(mununu, cwd, command, only_rows, budget_ms=None, timing=False):
    """Run the design's command with the row selection; return (facts by row key, stderr, rc)."""
    env = dict(os.environ)
    env["PATH"] = f"{mununu.parent}{os.pathsep}{env.get('PATH', '')}"
    env["MUNUNU_VERIFY_AUTO_ONLY_ROWS"] = only_rows
    if budget_ms is not None:
        env["MUNUNU_PROPERTY_BUDGET_MS"] = str(budget_ms)
    if timing:
        env["MUNUNU_PROPERTY_TIMING"] = "1"
    with tempfile.NamedTemporaryFile(suffix=".ndjson", delete=False) as tmp:
        facts_path = tmp.name
    env["MUNUNU_VERIFY_AUTO_ROW_FACTS"] = facts_path
    try:
        proc = subprocess.run(command, shell=True, cwd=cwd, env=env, capture_output=True, text=True)
        facts = {}
        text = Path(facts_path).read_text() if Path(facts_path).exists() else ""
        for line in text.splitlines():
            if line.strip():
                rec = json.loads(line)
                rec["row"] = rec.get("label") or rec["property"]
                facts[rec["row"]] = rec
        return facts, proc.stderr, proc.returncode
    finally:
        try:
            os.unlink(facts_path)
        except OSError:
            pass


def elapsed_by_name(stderr):
    out = {}
    for m in TIMING.finditer(stderr):
        out[m.group(1)] = out.get(m.group(1), 0) + int(m.group(2))
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--write", action="store_true", help="update verdicts.csv in place")
    ap.add_argument("--all", action="store_true", help="re-run every row (breadth), unknown rows included")
    ap.add_argument("--retired", action="append", default=[], metavar="ENGINE",
                    help="re-run rows whose recorded decided_by is this engine (repeatable)")
    ap.add_argument("--only", action="append", default=[], metavar="DESIGN", help="restrict to these designs")
    ap.add_argument("--mununu", help="the mununu binary")
    ap.add_argument("--ledger", default="measurements/ledger", help="the ledger directory")
    args = ap.parse_args()

    root = Path(__file__).resolve().parent.parent
    os.chdir(root)
    ledger_dir = Path(args.ledger)
    mununu = find_mununu(args.mununu)
    designs = read_designs(ledger_dir / "designs.tsv")
    if args.only:
        designs = [d for d in designs if d[0] in args.only]
    ledger = read_ledger(ledger_dir / "verdicts.csv")
    engine_rev = subprocess.run(["git", "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
    measured_at = dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    retired = set(args.retired)

    totals = {"designs": 0, "rows": 0, "run": 0, "contradictions": 0, "regressions": 0, "improved": 0, "errors": 0}
    updated = dict(ledger)
    for design, cwd, command in designs:
        totals["designs"] += 1
        facts, stderr, rc = run_design(mununu, cwd, command, only_rows="-")
        if not facts:
            totals["errors"] += 1
            print(f"== {design}: NO FACTS (exit {rc}); the design did not lift or the engine refused it")
            print("   " + "\n   ".join(stderr.strip().splitlines()[-6:]))
            continue
        totals["rows"] += len(facts)
        recorded = {row: r for (d, row), r in ledger.items() if d == design}
        gone = sorted(set(recorded) - set(facts))
        for row in gone:
            updated.pop((design, row), None)
        select, reasons = [], {}
        for row, rec in facts.items():
            old = recorded.get(row)
            if old is None:
                reasons[row] = "NEW"
            elif old["cone_hash"] != (rec.get("cone_hash") or ""):
                reasons[row] = "CHANGED"
            elif old["decided_by"] in retired:
                reasons[row] = "RETIRED"
            elif args.all:
                reasons[row] = "ALL"
            else:
                continue
            select.append(row)
        print(f"== {design}: {len(facts)} rows, {sum(1 for r in reasons.values() if r == 'NEW')} new, "
              f"{sum(1 for r in reasons.values() if r == 'CHANGED')} changed, "
              f"{sum(1 for r in reasons.values() if r == 'RETIRED')} retired, {len(gone)} gone "
              f"-> running {len(select)}")
        for row in gone:
            print(f"   GONE           {row}")
        if not select:
            continue
        budget = None
        recorded_ms = [int(recorded[r]["elapsed_ms"] or 0) for r in select if r in recorded and recorded[r]["elapsed_ms"]]
        if len(recorded_ms) == len(select) and recorded_ms:
            budget = min(BUDGET_CAP_MS, max(BUDGET_FLOOR_MS, 3 * max(recorded_ms)))
        else:
            budget = BUDGET_CAP_MS
        results, stderr, rc = run_design(mununu, cwd, command, only_rows=",".join(select), budget_ms=budget, timing=True)
        elapsed = elapsed_by_name(stderr)
        totals["run"] += len(select)
        for row in select:
            rec = results.get(row)
            if rec is None:
                totals["errors"] += 1
                print(f"   NO RESULT      {row} (exit {rc})")
                continue
            new_out = rec["outcome"]
            old = recorded.get(row)
            old_out = old["outcome"] if old else None
            verdict = reasons[row]
            if old_out in DEFINITE and new_out in DEFINITE and old_out != new_out:
                verdict, totals["contradictions"] = "CONTRADICTION", totals["contradictions"] + 1
            elif old_out in DEFINITE and new_out not in DEFINITE:
                verdict, totals["regressions"] = "REGRESSION", totals["regressions"] + 1
            elif old_out is not None and old_out not in DEFINITE and new_out in DEFINITE:
                verdict, totals["improved"] = "IMPROVED", totals["improved"] + 1
            ms = elapsed.get(rec["property"], 0)
            why = f"({rec.get('bottom_reason')})" if rec.get("bottom_reason") else ""
            by = f" by {rec['decided_by']}" if rec.get("decided_by") else ""
            arrow = f"{old_out} -> " if old_out else ""
            print(f"   {verdict:<14} {row}  {arrow}{new_out}{why}{by}  {ms / 1000:.1f} s")
            if verdict in ("CONTRADICTION", "REGRESSION"):
                continue  # a finding: the ledger keeps the measured row until a human decides
            updated[(design, row)] = {
                "design": design, "row": row, "property": rec["property"], "label": rec.get("label") or "",
                "cone_hash": rec.get("cone_hash") or "", "outcome": new_out,
                "decided_by": rec.get("decided_by") or "", "bottom_reason": rec.get("bottom_reason") or "",
                "elapsed_ms": str(ms), "measured_at": measured_at, "engine_rev": engine_rev,
            }
    if args.write:
        write_ledger(ledger_dir / "verdicts.csv", updated)
    print(f"ledger-check: {totals['designs']} designs, {totals['rows']} rows, {totals['run']} run, "
          f"{totals['contradictions']} contradictions, {totals['regressions']} regressions, "
          f"{totals['improved']} improved, {totals['errors']} errors; "
          f"{'verdicts.csv updated' if args.write else 'dry run (--write to record)'}")
    if totals["errors"]:
        return 2
    return 1 if totals["contradictions"] or totals["regressions"] else 0


if __name__ == "__main__":
    sys.exit(main())
