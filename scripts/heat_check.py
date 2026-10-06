#!/usr/bin/env python3
"""heat_check.py — did the hot spots move? A sub-10-minute profiling corpus, recorded and compared.

Run it at the START of every optimization experiment and AFTER every kept one. An optimization is
aimed at a hot spot measured on an earlier binary; once a prior change lands, the heat may have
moved and the candidate under study may no longer be worth doing. This records the callee map of
each corpus case (self and inclusive share per function, from a samply profile) and compares it
with the previous record, flagging any function whose share moved by more than a threshold and
reporting the watched functions — the ones the current candidate targets — by name.

Subcommands
  run  --label L [--bin BIN] [--corpus default|quick] [--compare PREV] [--watch FN ...]
       record the corpus → target/heat/<L>.json (+ the samply profiles under target/heat/<L>/),
       then compare against target/heat/<PREV>.json when given
  compare A B [--watch FN ...] [--threshold 10]
       compare two records

The corpus is a fixed set of calibrated-family cases sized at seconds each (about 90 s native in
total on the validation host). It covers the iteration-bound raster, the deep twocount latch, the
forward twocount32 shape on the owned portfolio, the representation-bound relational pair under
cell-major, a 16-bit multiplier, a real RTL control FSM, and the real i2c lift on the cube at
|P| = 8. `--corpus quick` halves it for a smoke run.

Binary: --bin, default target/heat/mununu (the `heat` cargo profile: release optimisation, no
LTO, line tables — builds in a few minutes and attributes frames better than the LTO build), then
target/profiling/mununu. Build it with `cargo build --profile heat -p mununu-cli`.

Reading the comparison. For each case: the top functions by SELF share before and after with the
delta, the top by INCLUSIVE share, the work counter and iteration count, and one of
  HEAT UNCHANGED   no function's share moved more than the threshold
  HEAT MOVED       something crossed it — re-read the candidate against the new map
Watched functions get their own line per case, so "is substitute still 60%?" is answered directly.

Exit codes: 0 = done; 2 = usage; 3 = a corpus case failed (record still written).
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import profile_cases as pc  # noqa: E402

REPO = pc.REPO
HEAT = REPO / "target" / "heat"

# (case, size, marker): the marker names the function that identifies the engine's thread in the
# profile — the owned portfolio runs the exact member on a detached thread.
CORPUS: dict[str, list[tuple[str, int, str]]] = {
    "default": [
        ("exact-raster", 800, "ExactModel::evaluate"),
        ("exact-twocount", 131072, "ExactModel::evaluate"),
        ("exact-forward", 65536, "exact_bad_reachable"),
        ("exact-relational", 10, "ExactModel::evaluate"),
        ("exact-mult", 16, "symbolic_bitblast"),
        ("exact-rtl-i2c", 0, "ExactModel::evaluate"),
        ("cube-rtl-i2c", 8, "predicate_cube_lift"),
    ],
    "quick": [
        ("exact-raster", 400, "ExactModel::evaluate"),
        ("exact-relational", 9, "ExactModel::evaluate"),
        ("cube-rtl-i2c", 6, "predicate_cube_lift"),
    ],
}


def find_bin(explicit: str | None) -> Path:
    for p in ([Path(explicit)] if explicit else []) + [REPO / "target" / "heat" / "mununu", REPO / "target" / "profiling" / "mununu"]:
        if p.exists():
            return p
    sys.exit("no binary: cargo build --profile heat -p mununu-cli")


def pick_thread(tables: dict[str, dict], marker: str) -> tuple[str, dict] | None:
    best = None
    for name, t in tables.items():
        if any(marker in f for f in t["incl"]):
            if best is None or t["samples"] > best[1]["samples"]:
                best = (name, t)
    return best


def record_case(case: str, size: int, marker: str, binary: Path, outdir: Path) -> dict:
    cmd, env, note = pc.command(case, size, binary)
    prof = outdir / f"{case}-{size}.samply.json.gz"
    t0 = time.perf_counter()
    proc = subprocess.run(["samply", "record", "--save-only", "-o", str(prof), "--", *cmd],
                          capture_output=True, text=True, env={**os.environ, "MUNUNU_BDD_REPORT_PEAK": "1", **env})
    wall = time.perf_counter() - t0
    work = sum(int(m.group(1)) for m in pc.WORK_RE.finditer(proc.stderr))
    iters = sum(int(m.group(2)) for m in pc.PEAK_RE.finditer(proc.stderr))
    rec = {"case": case, "size": size, "note": note, "wall_s": wall, "exit": proc.returncode,
           "work": work or None, "iterations": iters or None, "thread": None, "samples": 0, "self": {}, "incl": {}}
    if prof.exists():
        picked = pick_thread(pc.samply_tables(prof), marker)
        if picked:
            name, t = picked
            tot = t["samples"]
            rec["thread"] = name
            rec["samples"] = tot
            rec["self"] = {f: 100.0 * c / tot for f, c in sorted(t["self"].items(), key=lambda kv: -kv[1])[:40]}
            rec["incl"] = {f: 100.0 * c / tot for f, c in sorted(t["incl"].items(), key=lambda kv: -kv[1])[:40]}
    return rec


def short(f: str) -> str:
    import re
    f = re.sub(r"::h[0-9a-f]{16}", "", f)
    f = f.replace("mununu_core::adapter::btor2::", "").replace("oxidd_rules_bdd::simple::apply_rec::", "oxidd::").replace("oxidd_manager_index::manager::", "oxidd_mgr::")
    return f[:72]


def _norm_record(rec: dict) -> dict:
    """Strip LLVM per-build suffixes from function names (records made before the summariser did)."""
    import re
    pat = re.compile(r"\s*\(\.llvm\.\d+\)?|\.llvm\.\d+")
    for case in rec.get("cases", {}).values():
        for kind in ("self", "incl"):
            merged: dict[str, float] = {}
            for f, v in case.get(kind, {}).items():
                k = pat.sub("", f)
                merged[k] = merged.get(k, 0.0) + v
            case[kind] = dict(sorted(merged.items(), key=lambda kv: -kv[1]))
    return rec


def compare(a: dict, b: dict, watch: list[str], threshold: float) -> bool:
    a, b = _norm_record(a), _norm_record(b)
    print(f"\nheat check: A = {a['label']} ({a.get('commit')})   B = {b['label']} ({b.get('commit')})   threshold {threshold} points")
    moved_any = False
    for case, ra in a["cases"].items():
        rb = b["cases"].get(case)
        if not rb:
            print(f"\n== {case}: not in B"); continue
        print(f"\n== {case} (size {ra['size']})  wall {ra['wall_s']:.1f}s → {rb['wall_s']:.1f}s"
              + (f"  work {ra['work']:,} → {rb['work']:,}" if ra.get("work") and rb.get("work") else "")
              + (f"  iters {ra['iterations']:,} → {rb['iterations']:,}" if ra.get("iterations") and rb.get("iterations") else ""))
        moved = []
        for kind in ("self", "incl"):
            names = list(dict.fromkeys(list(ra[kind])[:8] + list(rb[kind])[:8]))
            print(f"   {kind:<5} {'A%':>6} {'B%':>6} {'Δ':>6}  function")
            for f in names[:10]:
                pa, pb = ra[kind].get(f, 0.0), rb[kind].get(f, 0.0)
                flag = "  <- MOVED" if abs(pb - pa) >= threshold else ""
                if flag:
                    moved.append((kind, f, pa, pb))
                print(f"   {'':<5} {pa:6.1f} {pb:6.1f} {pb - pa:+6.1f}  {short(f)}{flag}")
        for w in watch:
            fa = next((f for f in ra["incl"] if w in f), None)
            fb = next((f for f in rb["incl"] if w in f), None)
            ia, ib = (ra["incl"].get(fa, 0.0) if fa else 0.0), (rb["incl"].get(fb, 0.0) if fb else 0.0)
            sa, sb = (ra["self"].get(fa, 0.0) if fa else 0.0), (rb["self"].get(fb, 0.0) if fb else 0.0)
            rank_b = (list(rb["incl"]).index(fb) + 1) if fb else None
            print(f"   watch `{w}`: incl {ia:.1f}% → {ib:.1f}%  self {sa:.1f}% → {sb:.1f}%  rank by incl in B: {rank_b or '—'}")
        if moved:
            moved_any = True
            print(f"   HEAT MOVED on {case}: " + "; ".join(f"{k} {short(f)} {pa:.0f}→{pb:.0f}" for k, f, pa, pb in moved[:4]))
        else:
            print(f"   HEAT UNCHANGED on {case}")
    print("\nverdict:", "HEAT MOVED — re-read the candidate against the new map before building it" if moved_any else "HEAT UNCHANGED — the candidate's target is where it was")
    return moved_any


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(prog="heat_check.py", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="sub", required=True)
    r = sub.add_parser("run")
    r.add_argument("--label", required=True); r.add_argument("--bin", default=None); r.add_argument("--corpus", choices=CORPUS, default="default")
    r.add_argument("--compare", default=None, help="previous record label"); r.add_argument("--watch", nargs="*", default=[])
    r.add_argument("--threshold", type=float, default=10.0)
    c = sub.add_parser("compare"); c.add_argument("a"); c.add_argument("b"); c.add_argument("--watch", nargs="*", default=[]); c.add_argument("--threshold", type=float, default=10.0)
    args = p.parse_args(argv)
    if args.sub == "compare":
        a = json.loads((HEAT / f"{args.a}.json").read_text()); b = json.loads((HEAT / f"{args.b}.json").read_text())
        compare(a, b, args.watch, args.threshold); return 0
    if not shutil.which("samply"):
        sys.exit("samply not installed: brew install samply")
    binary = find_bin(args.bin)
    outdir = HEAT / args.label
    outdir.mkdir(parents=True, exist_ok=True)
    commit = subprocess.run(["git", "-C", str(REPO), "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
    dirty = bool(subprocess.run(["git", "-C", str(REPO), "status", "--porcelain", "--", "crates", "Cargo.toml"], capture_output=True, text=True).stdout.strip())
    rec = {"label": args.label, "bin": str(binary), "commit": commit + ("+dirty" if dirty else ""), "when": _dt.datetime.now(_dt.timezone.utc).isoformat(), "corpus": args.corpus, "cases": {}}
    t0 = time.perf_counter()
    failed = False
    print(f"heat check `{args.label}` on {binary} @ {rec['commit']}, corpus {args.corpus}")
    for case, size, marker in CORPUS[args.corpus]:
        cr = record_case(case, size, marker, binary, outdir)
        rec["cases"][case] = cr
        failed |= cr["exit"] not in (0, 1)
        top = next(iter(cr["self"]), "?")
        print(f"  {case:<18} size={size:<8} wall={cr['wall_s']:6.1f}s  work={cr['work'] or '—':>12}  thread={cr['thread']}  top self: {short(top)} {cr['self'].get(top, 0):.0f}%", flush=True)
    total = time.perf_counter() - t0
    rec["total_wall_s"] = total
    (HEAT / f"{args.label}.json").write_text(json.dumps(rec, indent=1))
    print(f"  total {total:.0f}s" + ("  ⚠ over the 10-minute budget — trim the corpus" if total > 600 else "") + f"  → {HEAT / (args.label + '.json')}")
    if args.compare:
        prev = json.loads((HEAT / f"{args.compare}.json").read_text())
        compare(prev, rec, args.watch, args.threshold)
    return 3 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
