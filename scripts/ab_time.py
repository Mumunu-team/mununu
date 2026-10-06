#!/usr/bin/env python3
"""ab_time.py — before/after timing for one command, with noise control and a verdict.

Criterion already does this for `cargo bench` targets. This tool is for everything else you
time by hand — a CLI verb, a `#[ignore]`d probe, a test filter — where "I ran it twice and it
looked faster" is the usual (and useless) evidence. It repeats the command, keeps every sample,
computes mean / standard deviation / median / coefficient of variation, and decides whether two
records differ by more than noise.

Subcommands
  preflight                              the quiet-laptop checklist, nothing else
  run <label> [opts] -- <cmd...>         warm-up, then N timed runs → <out>/<label>.json
  ab   [opts] --a '<cmd>' --b '<cmd>'    interleaved A B A B … runs, then compare
  compare <A.json> <B.json> [opts]       mean±sd, median, CV, ratio, Welch t-test, verdict

Common options
  -n N                  timed runs per command (default 7; 10+ for a claim you will publish)
  -w W                  warm-up runs, discarded (default 1; pays page-fault and cache costs)
  -o DIR                where records go (default target/ab_time, which git ignores)
  --wait-quiet S        poll the 1-minute load average for up to S seconds until it is below
                        --quiet-load before starting (default: do not wait)
  --quiet-load L        the load-average ceiling for --wait-quiet and the preflight (default 1.5)
  --caffeinate          wrap each run in `caffeinate -i` (macOS) so the machine does not idle-sleep
  --no-preflight        skip the checklist
  --threshold-percent P compare: the smallest change worth calling a change (default 5)
  --alpha A             compare: significance level for the Welch t-test (default 0.01)

Host-independent counters. The exact engine prints, under MUNUNU_BDD_REPORT_PEAK=1, a line
`[mununu#553] exact fixpoint: peak N … in K iteration(s)`; `sv verify-auto` prints, under
MUNUNU_PROPERTY_TIMING=1, `[mununu-timing] property=… elapsed_ms=…`. This tool parses both from
stderr and records iterations and peak nodes per run. When two records differ in ITERATIONS the
work changed and the wall clock is a consequence; when they match, you measured a per-iteration
cost change. Compare the counters before the clock.

Examples
  scripts/ab_time.py preflight
  MUNUNU_BDD_REPORT_PEAK=1 scripts/ab_time.py run before -n 10 --caffeinate -- \
      target/release/mununu --quiet btor2 verify-recoverability design.btor2 --target "state == 0"
  # … apply the change, rebuild …
  MUNUNU_BDD_REPORT_PEAK=1 scripts/ab_time.py run after -n 10 --caffeinate -- <same command>
  scripts/ab_time.py compare target/ab_time/before.json target/ab_time/after.json
  # strongest form: two binaries, interleaved so drift (thermal, background work) hits both equally
  scripts/ab_time.py ab -n 10 --a '/tmp/mununu-before --quiet btor2 verify x.btor2' \
                            --b '/tmp/mununu-after  --quiet btor2 verify x.btor2'

Exit codes: 0 = done; 2 = usage error; 3 = a timed run failed (record still written).
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import json
import math
import os
import platform
import re
import resource
import shlex
import socket
import statistics
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DEFAULT_OUT = REPO / "target" / "ab_time"

# Processes that commonly steal cycles on a developer laptop. Matched case-insensitively against
# the command name; browsers are flagged on sight, the rest only above --busy-cpu percent.
HEAVY_ALWAYS = ("google chrome", "chrome helper", "chromium", "firefox", "safari", "slack",
                "zoom.us", "microsoft teams", "discord", "spotify", "docker desktop", "com.docker")
HEAVY_IF_BUSY = ("cargo", "rustc", "clang", "cc1", "ld", "make", "ninja", "yosys", "z3", "cvc5",
                 "mdworker", "mds_stores", "backupd", "photoanalysisd", "node", "java", "python")

PEAK_RE = re.compile(r"exact fixpoint: peak (\d+) .*? in (\d+) iteration")
WORK_RE = re.compile(r"work (\d+) BDD ops")
TIMING_RE = re.compile(r"\[mununu-timing\] property=(\S+) outcome=(\S+) elapsed_ms=(\d+)")


# ----------------------------------------------------------------------------- provenance
def _sh(cmd: list[str]) -> str:
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=20).stdout.strip()
    except Exception:  # noqa: BLE001 — provenance is best-effort
        return ""


def provenance() -> dict:
    hw = ""
    fp = REPO / "scripts" / "capture_hw.sh"
    if fp.exists():
        hw = _sh(["bash", str(fp)])
    cpu = ""
    for line in hw.splitlines():
        if line.startswith("model.brand") or "model name" in line:
            cpu = line.split(None, 1)[1].strip() if " " in line else line
            break
    return {
        "commit": _sh(["git", "-C", str(REPO), "rev-parse", "HEAD"]),
        "commit_short": _sh(["git", "-C", str(REPO), "rev-parse", "--short", "HEAD"]),
        "branch": _sh(["git", "-C", str(REPO), "rev-parse", "--abbrev-ref", "HEAD"]),
        "git_dirty": bool(_sh(["git", "-C", str(REPO), "status", "--porcelain"])),
        "host": socket.gethostname(),
        "platform": platform.platform(),
        "cpu": cpu,
        "ncpu": os.cpu_count(),
        "python": sys.version.split()[0],
        "hw_fingerprint_sha256": hashlib.sha256(hw.encode()).hexdigest() if hw else "",
        "mununu_env": {k: v for k, v in os.environ.items() if k.startswith("MUNUNU_")},
    }


# ----------------------------------------------------------------------------- preflight
def loadavg() -> tuple[float, float, float]:
    try:
        return os.getloadavg()
    except OSError:
        return (0.0, 0.0, 0.0)


def heavy_processes(busy_cpu: float) -> tuple[list[str], list[str]]:
    """Returns (busy, idle_but_open): processes stealing cycles now, and known-heavy apps that
    are open but idle (they wake up on their own schedule, so the advice is still to quit them)."""
    out = _sh(["ps", "-axo", "pid=,pcpu=,comm="])
    busy, idle = [], set()
    for line in out.splitlines():
        parts = line.split(None, 2)
        if len(parts) < 3:
            continue
        pid, pcpu, comm = parts
        try:
            cpu = float(pcpu)
        except ValueError:
            continue
        name = os.path.basename(comm).lower()
        full = comm.lower()
        if any(h in full for h in HEAVY_ALWAYS):
            if cpu >= 1.0:
                busy.append(f"{name} (pid {pid}, {cpu:.0f}% cpu)")
            else:
                idle.add(next(h for h in HEAVY_ALWAYS if h in full))
        elif cpu >= busy_cpu and any(h in name for h in HEAVY_IF_BUSY):
            busy.append(f"{name} (pid {pid}, {cpu:.0f}% cpu)")
    return busy, sorted(idle)


def running_builds() -> str:
    """Count cargo / rustc / clang processes by exact command name (no -f, so a shell whose
    command line merely mentions cargo does not count)."""
    out = _sh(["ps", "-axo", "comm="])
    names = [os.path.basename(c.strip()).lower() for c in out.splitlines()]
    counts = {k: names.count(k) for k in ("cargo", "rustc", "clang", "cc1", "ld", "yosys", "z3")}
    hits = [f"{k}×{v}" for k, v in counts.items() if v]
    return ", ".join(hits)


def preflight(quiet_load: float, busy_cpu: float = 5.0) -> int:
    """Print the checklist. Returns the number of warnings."""
    warns = 0
    one, five, _ = loadavg()
    ncpu = os.cpu_count() or 1

    def row(ok: bool, what: str, detail: str) -> None:
        nonlocal warns
        tag = "ok  " if ok else "WARN"
        if not ok:
            warns += 1
        print(f"  {tag}  {what:<26} {detail}")

    print("preflight — the quiet-laptop checklist")
    row(one < quiet_load, "load average (1 min)", f"{one:.2f} on {ncpu} cpus (ceiling {quiet_load})")
    busy, idle = heavy_processes(busy_cpu)
    row(not busy, "busy heavy processes", "none" if not busy else "; ".join(busy[:6]))
    row(not idle, "heavy apps open but idle", "none" if not idle else ", ".join(idle) + " — quit them; they wake on their own")
    builds = running_builds()
    row(not builds, "cargo / rustc running", "none" if not builds else builds)
    if sys.platform == "darwin":
        batt = _sh(["pmset", "-g", "batt"])
        row("AC Power" in batt, "power", "on AC power" if "AC Power" in batt else "on battery — plug in")
        therm = _sh(["pmset", "-g", "therm"])
        m = re.search(r"CPU_Speed_Limit\s*=\s*(\d+)", therm)
        if m:
            lim = int(m.group(1))
            row(lim >= 100, "thermal throttling", f"CPU_Speed_Limit = {lim}")
        else:
            row(True, "thermal throttling", "not reported (pmset -g therm)")
        row(bool(_sh(["which", "caffeinate"])), "caffeinate available", "use --caffeinate to block idle sleep")
    else:
        ac = any(Path(p).read_text().strip() == "1" for p in Path("/sys/class/power_supply").glob("AC*/online")) \
            if Path("/sys/class/power_supply").exists() else True
        row(ac, "power", "on AC power" if ac else "on battery — plug in")
        gov = Path("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        if gov.exists():
            g = gov.read_text().strip()
            row(g in ("performance",), "cpu governor", f"{g} (performance is steadiest)")
    print()
    print("  before a measurement you intend to keep: quit browsers, Slack, Zoom, Docker Desktop and")
    print("  any IDE indexer; plug in power; wait for the 1-minute load to drop below", quiet_load, "(--wait-quiet 120);")
    print("  run at least 7 repeats (-n 7), 10+ for a published claim; interleave A and B (the `ab`")
    print("  subcommand) when both binaries exist; and compare the iteration / work counters before the clock.")
    return warns


def wait_quiet(seconds: int, quiet_load: float) -> None:
    t0 = time.time()
    while time.time() - t0 < seconds:
        one = loadavg()[0]
        if one < quiet_load:
            return
        print(f"  waiting for quiet: load {one:.2f} ≥ {quiet_load} ({int(time.time() - t0)}s)", file=sys.stderr)
        time.sleep(5)
    print(f"  gave up waiting after {seconds}s; measuring anyway (noise expected)", file=sys.stderr)


# ----------------------------------------------------------------------------- one run
def run_once(argv: list[str], caffeinate: bool, out_prefix: Path | None) -> dict:
    cmd = list(argv)
    if caffeinate and sys.platform == "darwin":
        cmd = ["caffeinate", "-i", *cmd]
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
    t0 = time.perf_counter()
    proc = subprocess.run(cmd, capture_output=True, text=True)
    wall = time.perf_counter() - t0
    r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    maxrss = r1.ru_maxrss if sys.platform == "darwin" else r1.ru_maxrss * 1024  # bytes
    iters, peak, work = 0, 0, 0
    for m in PEAK_RE.finditer(proc.stderr):
        peak = max(peak, int(m.group(1)))
        iters += int(m.group(2))
    for m in WORK_RE.finditer(proc.stderr):
        work += int(m.group(1))
    props = [{"property": p, "outcome": o, "elapsed_ms": int(ms)} for p, o, ms in TIMING_RE.findall(proc.stderr)]
    if out_prefix is not None:
        out_prefix.with_suffix(".out").write_text(proc.stdout)
        out_prefix.with_suffix(".err").write_text(proc.stderr)
    return {
        "wall_s": wall,
        "user_s": r1.ru_utime - r0.ru_utime,
        "sys_s": r1.ru_stime - r0.ru_stime,
        "maxrss_bytes_so_far": maxrss,  # max over all children so far in this process, not per run
        "exit_code": proc.returncode,
        "iterations": iters or None,
        "peak_nodes": peak or None,
        "work": work or None,
        "properties": props or None,
    }


# ----------------------------------------------------------------------------- statistics
def describe(xs: list[float]) -> dict:
    if not xs:
        return {}
    mean = statistics.fmean(xs)
    sd = statistics.stdev(xs) if len(xs) > 1 else 0.0
    return {
        "n": len(xs),
        "mean": mean,
        "sd": sd,
        "cv": (sd / mean) if mean else 0.0,
        "median": statistics.median(xs),
        "min": min(xs),
        "max": max(xs),
    }


def _betacf(a: float, b: float, x: float) -> float:
    # Continued fraction for the incomplete beta function (Numerical Recipes, betacf).
    maxit, eps, fpmin = 200, 3e-14, 1e-300
    qab, qap, qam = a + b, a + 1.0, a - 1.0
    c, d = 1.0, 1.0 - qab * x / qap
    d = 1.0 / (d if abs(d) > fpmin else fpmin)
    h = d
    for m in range(1, maxit + 1):
        m2 = 2 * m
        aa = m * (b - m) * x / ((qam + m2) * (a + m2))
        d = 1.0 + aa * d
        d = 1.0 / (d if abs(d) > fpmin else fpmin)
        c = 1.0 + aa / (c if abs(c) > fpmin else fpmin)
        h *= d * c
        aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2))
        d = 1.0 + aa * d
        d = 1.0 / (d if abs(d) > fpmin else fpmin)
        c = 1.0 + aa / (c if abs(c) > fpmin else fpmin)
        de = d * c
        h *= de
        if abs(de - 1.0) < eps:
            break
    return h


def betai(a: float, b: float, x: float) -> float:
    """Regularized incomplete beta I_x(a, b)."""
    if x <= 0.0:
        return 0.0
    if x >= 1.0:
        return 1.0
    lbeta = math.lgamma(a + b) - math.lgamma(a) - math.lgamma(b)
    bt = math.exp(lbeta + a * math.log(x) + b * math.log(1.0 - x))
    if x < (a + 1.0) / (a + b + 2.0):
        return bt * _betacf(a, b, x) / a
    return 1.0 - bt * _betacf(b, a, 1.0 - x) / b


def welch(a: list[float], b: list[float]) -> dict:
    na, nb = len(a), len(b)
    if na < 2 or nb < 2:
        return {"t": None, "df": None, "p": None}
    ma, mb = statistics.fmean(a), statistics.fmean(b)
    va, vb = statistics.variance(a), statistics.variance(b)
    se2 = va / na + vb / nb
    if se2 == 0.0:
        return {"t": 0.0, "df": float(na + nb - 2), "p": 1.0 if ma == mb else 0.0}
    t = (mb - ma) / math.sqrt(se2)
    df = se2 ** 2 / ((va / na) ** 2 / (na - 1) + (vb / nb) ** 2 / (nb - 1))
    p = betai(df / 2.0, 0.5, df / (df + t * t))  # two-sided
    return {"t": t, "df": df, "p": p}


def mann_whitney(a: list[float], b: list[float]) -> dict:
    """Two-sided Mann–Whitney U with the normal approximation (ties ignored); robust to outliers."""
    na, nb = len(a), len(b)
    if na < 3 or nb < 3:
        return {"u": None, "p": None}
    u = sum(1.0 if x < y else 0.5 if x == y else 0.0 for x in a for y in b)
    mu = na * nb / 2.0
    sigma = math.sqrt(na * nb * (na + nb + 1) / 12.0)
    z = (u - mu) / sigma if sigma else 0.0
    p = math.erfc(abs(z) / math.sqrt(2.0))
    return {"u": u, "p": p}


# ----------------------------------------------------------------------------- records
def record(label: str, argv: list[str], samples: list[dict], warmups: int, prov: dict,
           load_before: tuple, load_after: tuple) -> dict:
    walls = [s["wall_s"] for s in samples]
    users = [s["user_s"] for s in samples]
    iters = [s["iterations"] for s in samples if s["iterations"] is not None]
    works = [s.get("work") for s in samples if s.get("work") is not None]
    return {
        "schema": "ab_time/1",
        "label": label,
        "command": argv,
        "command_str": shlex.join(argv),
        "cwd": os.getcwd(),
        "n": len(samples),
        "warmup": warmups,
        "started_at": prov.pop("_started_at"),
        "ended_at": _dt.datetime.now(_dt.timezone.utc).isoformat(),
        "load_before": load_before,
        "load_after": load_after,
        "samples": samples,
        "stats": {
            "wall_s": describe(walls),
            "user_s": describe(users),
            "iterations": describe([float(i) for i in iters]) if iters else None,
            "work": describe([float(w) for w in works]) if works else None,
        },
        "any_failed": any(s["exit_code"] != 0 for s in samples),
        "provenance": prov,
    }


def fmt_stats(st: dict, unit: str = "s") -> str:
    if not st:
        return "—"
    return (f"mean {st['mean']:.4f}{unit} ± {st['sd']:.4f} (cv {st['cv'] * 100:.1f}%)  "
            f"median {st['median']:.4f}  min {st['min']:.4f}  max {st['max']:.4f}  n={st['n']}")


def print_record(rec: dict) -> None:
    print(f"\n{rec['label']}: {rec['command_str']}")
    print(f"  wall  {fmt_stats(rec['stats']['wall_s'])}")
    print(f"  user  {fmt_stats(rec['stats']['user_s'])}")
    if rec["stats"]["iterations"]:
        it = rec["stats"]["iterations"]
        same = it["min"] == it["max"]
        print(f"  iterations {int(it['min'])}" + ("" if same else f"–{int(it['max'])} (VARIES between runs — not deterministic?)"))
    if rec["any_failed"]:
        print("  WARNING: at least one run exited non-zero; see the .err files")


def do_run(label: str, argv: list[str], args: argparse.Namespace, out: Path) -> dict:
    out.mkdir(parents=True, exist_ok=True)
    prov = provenance()
    prov["_started_at"] = _dt.datetime.now(_dt.timezone.utc).isoformat()
    lb = loadavg()
    for i in range(args.warmup):
        print(f"  [{label}] warm-up {i + 1}/{args.warmup}", file=sys.stderr)
        run_once(argv, args.caffeinate, None)
    samples = []
    for i in range(args.n):
        s = run_once(argv, args.caffeinate, out / f"{label}.run{i + 1}")
        samples.append(s)
        extra = f" iters={s['iterations']}" if s["iterations"] else ""
        print(f"  [{label}] run {i + 1}/{args.n}: {s['wall_s']:.4f}s exit={s['exit_code']}{extra}", file=sys.stderr)
    rec = record(label, argv, samples, args.warmup, prov, lb, loadavg())
    path = out / f"{label}.json"
    path.write_text(json.dumps(rec, indent=2))
    print_record(rec)
    print(f"  → {path}")
    return rec


def _metric_block(a: dict, b: dict, key: str, label: str) -> dict:
    xa = [s[key] for s in a["samples"]]
    xb = [s[key] for s in b["samples"]]
    sa, sb = describe(xa), describe(xb)
    ratio_mean = sb["mean"] / sa["mean"] if sa["mean"] else float("inf")
    ratio_median = sb["median"] / sa["median"] if sa["median"] else float("inf")
    w, mw = welch(xa, xb), mann_whitney(xa, xb)
    print(f"  A {label:<5} {fmt_stats(sa)}")
    print(f"  B {label:<5} {fmt_stats(sb)}")
    direction = f"{(1 - ratio_mean) * 100:.1f}% faster" if ratio_mean <= 1 else f"{(ratio_mean - 1) * 100:.1f}% slower"
    line = f"    B/A mean ratio {ratio_mean:.3f}  median ratio {ratio_median:.3f}  ({direction} by mean)"
    if w["p"] is not None:
        line += f"  Welch t={w['t']:.2f} df={w['df']:.1f} p={w['p']:.2g}"
    if mw["p"] is not None:
        line += f"  Mann–Whitney p={mw['p']:.2g}"
    print(line)
    return {"a": sa, "b": sb, "ratio_mean": ratio_mean, "welch": w, "mw": mw}


def do_compare(a: dict, b: dict, threshold_percent: float, alpha: float, metric: str = "wall") -> int:
    print(f"\ncompare  A = {a['label']}  ({a['command_str']})")
    print(f"         B = {b['label']}  ({b['command_str']})")
    if a["command_str"] != b["command_str"]:
        print("  note: the commands differ — fine for two binaries or two env settings, suspicious otherwise")
    if a["provenance"].get("commit") != b["provenance"].get("commit"):
        print(f"  commits: A {a['provenance'].get('commit_short')}  B {b['provenance'].get('commit_short')}")
    wall = _metric_block(a, b, "wall_s", "wall")
    user = _metric_block(a, b, "user_s", "user")
    wa_, wb_ = a["stats"].get("work"), b["stats"].get("work")
    if wa_ and wb_:
        if wa_["mean"] != wb_["mean"]:
            print(f"  WORK CHANGED: BDD ops {int(wa_['mean']):,} → {int(wb_['mean']):,} ({wb_['mean'] / wa_['mean']:.3f}×). "
                  f"Host-independent — cite this before any clock.")
        else:
            print(f"  BDD ops identical ({int(wa_['mean']):,})")
    ia, ib = a["stats"].get("iterations"), b["stats"].get("iterations")
    if ia and ib:
        if ia["mean"] != ib["mean"]:
            print(f"  WORK CHANGED: iterations {int(ia['mean'])} → {int(ib['mean'])} "
                  f"({ib['mean'] / ia['mean']:.3f}×). Host-independent — this is the evidence to cite.")
        else:
            print(f"  iterations identical ({int(ia['mean'])}): the change is per-iteration cost, not work")
    chosen = wall if metric == "wall" else user
    sa, sb, w, ratio_mean = chosen["a"], chosen["b"], chosen["welch"], chosen["ratio_mean"]
    noisy = max(sa["cv"], sb["cv"]) > 0.10
    thr = threshold_percent / 100.0
    if noisy:
        verdict = (f"TOO NOISY on {metric} — cv > 10% in A or B; quiet the machine (preflight), raise -n, "
                   f"use `ab` to interleave")
        if metric == "wall" and max(user["a"]["cv"], user["b"]["cv"]) <= 0.10:
            verdict += "; user CPU time is steady here, so --metric user is usable for a CPU-bound command"
    elif w["p"] is not None and w["p"] < alpha and abs(ratio_mean - 1.0) > thr:
        verdict = f"FASTER by {(1 - ratio_mean) * 100:.1f}%" if ratio_mean < 1 else f"SLOWER by {(ratio_mean - 1) * 100:.1f}%"
        verdict += f" on {metric} (p < {alpha}, beyond the {threshold_percent}% threshold)"
    elif w["p"] is not None and w["p"] < alpha:
        verdict = f"statistically distinct on {metric} but within the {threshold_percent}% threshold — not worth a claim"
    else:
        verdict = f"NO SIGNIFICANT CHANGE on {metric} at alpha {alpha}"
    print(f"  verdict: {verdict}")
    return 0


# ----------------------------------------------------------------------------- cli
def common(p: argparse.ArgumentParser) -> None:
    p.add_argument("-n", type=int, default=7, dest="n")
    p.add_argument("-w", type=int, default=1, dest="warmup")
    p.add_argument("-o", type=Path, default=DEFAULT_OUT, dest="out")
    p.add_argument("--wait-quiet", type=int, default=0)
    p.add_argument("--quiet-load", type=float, default=1.5)
    p.add_argument("--caffeinate", action="store_true")
    p.add_argument("--no-preflight", action="store_true")


def main(argv: list[str]) -> int:
    top = argparse.ArgumentParser(prog="ab_time.py", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = top.add_subparsers(dest="sub", required=True)

    pp = sub.add_parser("preflight", help="the quiet-laptop checklist")
    pp.add_argument("--quiet-load", type=float, default=1.5)

    pr = sub.add_parser("run", help="warm-up then N timed runs of one command (after `--`)")
    pr.add_argument("label")
    common(pr)

    pab = sub.add_parser("ab", help="interleaved A/B runs of two commands, then compare")
    common(pab)
    pab.add_argument("--a", required=True, help="command A as one quoted string")
    pab.add_argument("--b", required=True, help="command B as one quoted string")
    pab.add_argument("--a-label", default="A")
    pab.add_argument("--b-label", default="B")
    pab.add_argument("--threshold-percent", type=float, default=5.0)
    pab.add_argument("--alpha", type=float, default=0.01)
    pab.add_argument("--metric", choices=("wall", "user"), default="wall")

    pc = sub.add_parser("compare", help="compare two records")
    pc.add_argument("a", type=Path)
    pc.add_argument("b", type=Path)
    pc.add_argument("--threshold-percent", type=float, default=5.0)
    pc.add_argument("--alpha", type=float, default=0.01)
    pc.add_argument("--metric", choices=("wall", "user"), default="wall",
                    help="which time the verdict is on; user CPU time is steadier on a loaded machine")

    # `run` takes the command after a literal `--`; split it off before argparse sees it, because
    # argparse's REMAINDER would swallow the run options too.
    run_cmd: list[str] = []
    if argv[:1] == ["run"] and "--" in argv:
        i = argv.index("--")
        argv, run_cmd = argv[:i], argv[i + 1:]

    args = top.parse_args(argv)

    if args.sub == "preflight":
        return 1 if preflight(args.quiet_load) else 0

    if args.sub == "compare":
        return do_compare(json.loads(args.a.read_text()), json.loads(args.b.read_text()),
                          args.threshold_percent, args.alpha, args.metric)

    if not args.no_preflight:
        preflight(args.quiet_load)
        print()
    if args.wait_quiet:
        wait_quiet(args.wait_quiet, args.quiet_load)

    if args.sub == "run":
        if not run_cmd:
            top.error("run: give the command after a literal `--`")
        rec = do_run(args.label, run_cmd, args, args.out)
        return 3 if rec["any_failed"] else 0

    # ab: interleave, so slow drift affects both commands equally.
    ca, cb = shlex.split(args.a), shlex.split(args.b)
    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    prov_a, prov_b = provenance(), provenance()
    prov_a["_started_at"] = prov_b["_started_at"] = _dt.datetime.now(_dt.timezone.utc).isoformat()
    lb = loadavg()
    for i in range(args.warmup):
        print(f"  warm-up {i + 1}/{args.warmup} (A then B)", file=sys.stderr)
        run_once(ca, args.caffeinate, None)
        run_once(cb, args.caffeinate, None)
    sa_, sb_ = [], []
    for i in range(args.n):
        for label, cmd, acc in ((args.a_label, ca, sa_), (args.b_label, cb, sb_)):
            s = run_once(cmd, args.caffeinate, out / f"{label}.run{i + 1}")
            acc.append(s)
            print(f"  [{label}] run {i + 1}/{args.n}: {s['wall_s']:.4f}s exit={s['exit_code']}", file=sys.stderr)
    la = loadavg()
    ra = record(args.a_label, ca, sa_, args.warmup, prov_a, lb, la)
    rb = record(args.b_label, cb, sb_, args.warmup, prov_b, lb, la)
    (out / f"{args.a_label}.json").write_text(json.dumps(ra, indent=2))
    (out / f"{args.b_label}.json").write_text(json.dumps(rb, indent=2))
    print_record(ra)
    print_record(rb)
    do_compare(ra, rb, args.threshold_percent, args.alpha, args.metric)
    return 3 if (ra["any_failed"] or rb["any_failed"]) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
