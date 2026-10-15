#!/usr/bin/env python3
"""bitcap_frontier.py — where the exact engine stops deciding, per design shape, under DEFAULT budgets.

"How many bits can the exact engine handle?" has no single answer. The admission cap is 40 bits
by default and is raised to fit the cone up to 192 (`default_cap_for_cone`,
crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs); the recoverability verb retries a
bit-cap abstention once at the 1024-bit ceiling (the P3 cap ladder); and what actually stops a run
is the SHAPE of the cone — a multiplier's BDD is exponential at every width (Bryant 1986), a
counter's or an FSM timer's is linear. This script measures, per shape, the largest width the exact
engine decides (verdict `holds` / `violated` with `engine` = `exact` or `exact-reach-restricted`)
and the first width it does not, with WHY — the typed `exact_abstention` the verbs report since
mununu#682 (`bit-cap`, `arena-overflow`, `iteration-budget`, `latency`, `arena-safety`, …), or the
host wall cap — by bisecting on a width knob whose cone grows monotonically.

It reuses the generators of `profile_cases.py` and its defaults mode: every probe runs the RELEASE
binary with every `MUNUNU_*` variable scrubbed from the shell, so the numbers are the shipped
defaults, not a tuned run (the profiling families turn the soft bounds OFF; this script never
does). The deterministic bounds reproduce across hosts; a wall cap is this host's and is labelled
so. Every probe is kept — the arena tier steps at 40 bits, so decide/abstain is not guaranteed
monotone and a non-monotone pair is flagged, never averaged away.

    scripts/bitcap_frontier.py smoke                       # ≈ 1–2 min: the known points, asserted
    scripts/bitcap_frontier.py sweep                       # every shape × its configurations (hours)
    scripts/bitcap_frontier.py sweep --shapes fsm-timeout,mult --cap 300 --max-probes 8
    scripts/bitcap_frontier.py sweep --shapes raster,twocount --config squaring-off
    scripts/bitcap_frontier.py table target/frontier/frontier.json
    scripts/bitcap_frontier.py record EXP-0031-bitcap-frontier   # archive the JSON with provenance

Sweeps MERGE into `target/frontier/frontier.json` by (shape, configuration), so a long sweep can run
one shape at a time (`--fresh` starts over). `make frontier-smoke` builds the release binary and
runs `smoke`; it is not part of `make ci`.
"""
from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

sys.path.insert(0, str(Path(__file__).resolve().parent))
import profile_cases as pc  # noqa: E402  (the generators, CASES, command(), run_timed())

REPO = pc.REPO
OUT_DIR = REPO / "target" / "frontier"
DEFAULT_OUT = OUT_DIR / "frontier.json"
RELEASE_BIN = REPO / "target" / "release" / "mununu"
AUTO_CAP_CEILING = 192      # mirrors symbolic_bitblast.rs — above it the cap ladder retries at 1024
HARD_CEILING = 1024


# ----------------------------------------------------------------------------- shapes
def _pow2(k: int) -> int:
    return 1 << k


@dataclass(frozen=True)
class Shape:
    case: str                              # key into profile_cases.CASES
    knob: str                              # what k means
    size_of: Callable[[int], int]          # k → the generator's size argument
    cone_of: Callable[[int], int | None]   # k → kept register+input bits (None: read `kept_bits`)
    seeds: tuple[int, ...]                 # ascending k; the walk stops at the first non-decider
    expected: str | None                   # the verdict a DECIDED probe must carry; None = either
    configs: tuple[str, ...] = ("default",)
    fixed: bool = False                    # a single RTL fixture, one probe

    def ladder_k(self) -> int | None:
        """The smallest seed whose cone exceeds the auto-cap ceiling — the row that exercises the
        cap ladder's retry at 1024 bits."""
        for k in self.seeds:
            c = self.cone_of(k)
            if c is not None and c > AUTO_CAP_CEILING:
                return k
        return None


# Cone formulas are the generators' (checked against the engine's `kept_bits` on every probe; a
# mismatch is reported in the table). Seeds bracket 40 (the arena tier), 192 (the auto-cap
# ceiling) and 1024 (the hard ceiling) where the shape can reach them.
SHAPES: dict[str, Shape] = {
    "fsm-timeout": Shape("exact-fsm-timeout", "W: timer + limit width", lambda k: k, lambda k: 2 * k + 6,
                         (8, 16, 17, 32, 64, 93, 94, 128, 256, 509, 510), "holds"),
    # The stuck twin never reads `clr` (ERROR is absorbing), so the bit-level cone of influence drops that
    # input: 2W+5, one bit fewer than its HOLDS twin — the engine's `kept_bits` confirmed it (2026-10-14).
    "fsm-timeout-stuck": Shape("exact-fsm-timeout-stuck", "W: timer + limit width", lambda k: k, lambda k: 2 * k + 5,
                               (8, 16, 17, 32, 64, 93, 94, 128, 256, 509, 510), "violated"),
    "fsm-pair": Shape("exact-fsm-pair", "W: timer + limit width", lambda k: k, lambda k: 3 * k + 9,
                      (8, 10, 11, 16, 32, 61, 62, 128, 256, 338, 339), "holds"),
    "mult": Shape("exact-mult", "n: bits per operand", lambda k: k, lambda k: 2 * k + 1,
                  (8, 16, 20, 24, 32, 40, 48, 64, 96), "violated"),
    "relational": Shape("exact-relational", "n: bits per register", lambda k: k, lambda k: 2 * k + 1,
                        (8, 16, 20, 32, 64, 95, 96, 128, 256, 511, 512), "violated", configs=("default", "cell-major")),
    # twocount's wide `constd m-1` parses at any width (mununu#650 folds a decimal past i128 to hex) and
    # its target is `done == 1`, so it reaches the 1024-bit ceiling.
    "twocount": Shape("exact-twocount", "w: bits per counter (m = 2^w)", _pow2, lambda k: 2 * k + 2,
                      (8, 12, 16, 19, 20, 24, 32, 64, 95, 96, 128, 256, 511, 512), "holds", configs=("default", "squaring-off")),
    # raster's TARGET is `vcount == v-1`, a predicate literal the atom parser folds through a fixed-width
    # integer; past it the probe is UNRUNNABLE (reported as such, never as a frontier). The 192-bit ladder
    # row (vb = 183) is therefore out of this generator's reach and the table says so.
    "raster": Shape("exact-raster", "vb: vcount bits (v = 2^vb, h = 800)", _pow2, lambda k: 10 + k,
                    (4, 8, 10, 12, 15, 16, 30, 31, 62, 63, 64, 126, 127, 128, 183), "holds", configs=("default", "squaring-off")),
    "i2c": Shape("exact-rtl-i2c", "(fixed RTL: i2c scl_padoen)", lambda k: 0, lambda k: None, (0,), "holds", fixed=True),
    "aes": Shape("exact-rtl-aes", "(fixed RTL: aes cipher control FSM)", lambda k: 0, lambda k: None, (0,), None, fixed=True),
}

# Labelled toggles. `default` is the shipped engine; the others exhibit a documented cliff.
CONFIGS: dict[str, dict[str, str]] = {
    "default": {},
    "squaring-off": {"MUNUNU_BDD_SQUARING": "0"},          # #611's rescue off: the iteration frontier reappears
    "cell-major": {"MUNUNU_BDD_VAR_ORDER": "cell-major"},   # the opt-out order: the representation cliff
}

DECIDED = ("holds", "violated")


# ----------------------------------------------------------------------------- one probe
def decided_exact(r: dict) -> bool:
    return r["verdict"] in DECIDED and not r["timed_out"] and (r.get("engine") or "").startswith("exact")


def bounded_by(r: dict, cap: float) -> str | None:
    """What stopped the exact engine on a probe that did not decide. Wall caps are the host's;
    everything else is a deterministic budget and reproduces."""
    if decided_exact(r):
        return None
    if r["timed_out"]:
        why = f"wall cap {cap:g} s (host)"
        if r.get("abstained_on"):
            why += f"; the exact engine had abstained first: {r['abstained_on']}"
        elif r.get("escalated"):
            why += "; the exact engine had abstained first (cube ladder running)"
        return why
    if r.get("exact_abstention"):
        return r["exact_abstention"]
    if r.get("abstained_on"):
        return r["abstained_on"]
    if (r.get("engine") or "") == "cube-ladder":
        return "cube-ladder (no exact abstention reported — rebuild the binary? mununu#682)"
    if r["verdict"] in DECIDED:
        return f"decided by {r.get('engine') or 'an unreported engine'}"
    return f"undecided ({r['verdict']}), exit {r['exit']}"


def probe(shape_name: str, k: int, config: str, binary: Path, cap: float) -> dict:
    sh = SHAPES[shape_name]
    cmd, _case_env, note = pc.command(sh.case, sh.size_of(k), binary)
    r = pc.run_timed(cmd, {}, cap, defaults=True, extra_env=CONFIGS[config])
    rec = {"shape": shape_name, "config": config, "k": k, "size": sh.size_of(k),
           "cone_bits": sh.cone_of(k), "note": note, "cmd": cmd, **r}
    rec["decided_exact"] = decided_exact(r)
    rec["decided"] = r["verdict"] in DECIDED and not r["timed_out"]
    # The binary refused the INPUT (a generator or atom-parser limit, exit 1 with no verdict): not a
    # frontier reading. Exit 2 is a VIOLATED verdict's CI exit code, so only a verdict-less exit counts.
    rec["unrunnable"] = (not r["timed_out"] and r["exit"] not in (0, 2)
                         and r["verdict"] not in DECIDED and r["verdict"] != "unknown")
    rec["verdict_mismatch"] = bool(sh.expected and rec["decided"] and r["verdict"] != sh.expected)
    rec["cone_mismatch"] = bool(rec["cone_bits"] is not None and r.get("kept_bits") is not None
                                and r["kept_bits"] != rec["cone_bits"])
    rec["bounded_by"] = bounded_by(r, cap)
    return rec


def fmt_probe(p: dict) -> str:
    cone = p["cone_bits"] if p["cone_bits"] is not None else p.get("kept_bits")
    kept = f"/{p['kept_bits']}" if p.get("kept_bits") is not None and p["cone_bits"] is not None else ""
    tail = ""
    if p["unrunnable"]:
        tail = f"UNRUNNABLE (exit {p['exit']}) — generator / atom-parser limit, not a frontier: …{p['stderr_tail'][-160:].strip()}"
    elif p["decided_exact"]:
        tail = f"DECIDED {p['verdict']} by {p['engine']}"
    else:
        tail = f"not decided by exact — verdict={p['verdict']} engine={p.get('engine')} bounded_by={p['bounded_by']}"
    flags = (" ⚠ VERDICT MISMATCH" if p["verdict_mismatch"] else "") + (" ⚠ CONE MISMATCH" if p["cone_mismatch"] else "")
    its = f" iters={p['iterations']}" if p.get("iterations") else ""
    pk = f" peak={p['peak_nodes']:,}" if p.get("peak_nodes") else ""
    return (f"  {p['shape']:<18} {p['config']:<12} k={p['k']:<5} cone={cone}{kept}b  wall={p['wall_s']:7.1f}s"
            f"{its}{pk}  {tail}{flags}")


# ----------------------------------------------------------------------------- the frontier of one (shape, config)
def frontier(shape_name: str, config: str, binary: Path, cap: float, max_probes: int, log) -> dict:
    sh = SHAPES[shape_name]
    probes: list[dict] = []

    def run(k: int) -> dict:
        p = probe(shape_name, k, config, binary, cap)
        probes.append(p)
        log(fmt_probe(p))
        return p

    if sh.fixed:
        run(0)
    else:
        last_ok: int | None = None
        first_bad: int | None = None
        # 1. Walk the seeds to the first non-decider. The walk is not budgeted: a deciding probe is
        #    fast by definition (it decided within the cap), and the seeds are the fixed rows the
        #    table wants (the 40-bit arena tier, the 192-bit auto-cap, the 1024-bit ceiling).
        for k in sh.seeds:
            p = run(k)
            if p["unrunnable"]:
                break                                        # the generator's reach ends here, not the engine's
            if p["decided_exact"]:
                last_ok = k
            else:
                first_bad = k
                break
        walked = len(probes)
        extra = lambda: len(probes) - walked                     # noqa: E731  probes spent after the walk
        # 2. Bisect to adjacency, within `max_probes` extra probes.
        while (last_ok is not None and first_bad is not None and first_bad - last_ok > 1
               and extra() < max_probes):
            mid = (last_ok + first_bad) // 2
            if run(mid)["decided_exact"]:
                last_ok = mid
            else:
                first_bad = mid
        # 3. One look past the first non-decider: the arena tier steps at 40 bits, so a wider cone can
        #    decide where a narrower one abstained; that is reported, not averaged away.
        if first_bad is not None and extra() < max_probes:
            probed = {p["k"] for p in probes}
            beyond = [k for k in sh.seeds if k > first_bad and k not in probed]
            if beyond:
                run(beyond[0])
        # 4. The cap-ladder row (cone > 192), always — the retry at the 1024-bit ceiling.
        lk = sh.ladder_k()
        if lk is not None and not any(p["k"] == lk for p in probes) and not any(p["unrunnable"] for p in probes):
            run(lk)

    runnable = [p for p in probes if not p["unrunnable"]]
    ok = [p for p in runnable if p["decided_exact"]]
    bad = [p for p in runnable if not p["decided_exact"]]
    best = max(ok, key=lambda p: p["k"]) if ok else None
    first = min(bad, key=lambda p: p["k"]) if bad else None
    non_monotone = bool(ok and bad and any(b["k"] < o["k"] for b in bad for o in ok))
    ladder = [p for p in probes if p["cone_bits"] is not None and p["cone_bits"] > AUTO_CAP_CEILING]
    return {
        "shape": shape_name, "config": config, "case": sh.case, "knob": sh.knob, "expected": sh.expected,
        "env": CONFIGS[config], "cap_s": cap, "max_probes": max_probes,
        "largest_deciding": best, "first_non_deciding": first,
        "ladder_row": min(ladder, key=lambda p: p["k"]) if ladder else None,
        "non_monotone": non_monotone,
        "unrunnable": [(p["k"], p["cone_bits"], p["stderr_tail"][-200:]) for p in probes if p["unrunnable"]],
        "verdict_mismatches": [p["k"] for p in probes if p["verdict_mismatch"]],
        "cone_mismatches": [(p["k"], p["cone_bits"], p["kept_bits"]) for p in probes if p["cone_mismatch"]],
        "probes": sorted(probes, key=lambda p: p["k"]),
    }


# ----------------------------------------------------------------------------- provenance
def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def git(*args: str) -> str:
    return subprocess.run(["git", "-C", str(REPO), *args], capture_output=True, text=True).stdout.strip()


def provenance(binary: Path, argv: list[str]) -> dict:
    return {
        "commit": git("rev-parse", "HEAD"), "commit_short": git("rev-parse", "--short", "HEAD"),
        "branch": git("rev-parse", "--abbrev-ref", "HEAD"),
        "git_dirty": "yes" if git("status", "--porcelain") else "no",
        "host": platform.node(), "platform": platform.platform(), "machine": platform.machine(),
        "container": "yes" if Path("/.dockerenv").exists() else "no",
        "python": platform.python_version(),
        "binary": str(binary), "binary_sha256": sha256_of(binary),
        "binary_mtime": _dt.datetime.fromtimestamp(binary.stat().st_mtime, _dt.timezone.utc).isoformat(),
        "mununu_env_scrubbed": sorted(k for k in os.environ if k.startswith("MUNUNU_")),
        "rust_log": pc.DEFAULTS_RUST_LOG,
        "argv": argv,
    }


def loadavg() -> list[float] | None:
    try:
        return [round(x, 2) for x in os.getloadavg()]
    except (OSError, AttributeError):
        return None


def resolve_bin(explicit: str | None) -> Path:
    p = Path(explicit) if explicit else RELEASE_BIN
    if not p.exists():
        sys.exit(f"no binary at {p}: build it with `cargo build --release -p mununu-cli` (or `make build`)")
    return p


# ----------------------------------------------------------------------------- sweep
def sweep(args, argv: list[str]) -> int:
    binary = resolve_bin(args.bin)
    shapes = args.shapes.split(",") if args.shapes else list(SHAPES)
    unknown = [s for s in shapes if s not in SHAPES]
    if unknown:
        sys.exit(f"unknown shape(s) {unknown}; known: {', '.join(SHAPES)}")
    out = Path(args.out)
    doc = {"schema": "bitcap-frontier/1", "results": []}
    if out.exists() and not args.fresh:
        doc = json.loads(out.read_text())
    la = loadavg()
    if la and la[0] > args.quiet_load:
        print(f"⚠ 1-min load {la[0]} > {args.quiet_load}: wall times will be inflated (the deterministic "
              f"bounds still reproduce). Pass --quiet-load to change the threshold.", file=sys.stderr)
    started = _dt.datetime.now(_dt.timezone.utc)
    plan = []
    for s in shapes:
        cfgs = [args.config] if args.config else SHAPES[s].configs
        for c in cfgs:
            if c not in CONFIGS:
                sys.exit(f"unknown config {c}; known: {', '.join(CONFIGS)}")
            if c in SHAPES[s].configs or args.config:
                plan.append((s, c))
    print(f"frontier sweep: {len(plan)} (shape, config) pairs, cap {args.cap:g} s/probe, ≤{args.max_probes} probes each, "
          f"binary {binary} ({sha256_of(binary)[:12]}), load {la}")
    for s, c in plan:
        print(f"\n== {s} [{c}]  knob: {SHAPES[s].knob}  expected: {SHAPES[s].expected or 'either'}")
        res = frontier(s, c, binary, args.cap, args.max_probes, print)
        res["when"] = _dt.datetime.now(_dt.timezone.utc).isoformat()
        doc["results"] = [r for r in doc["results"] if (r["shape"], r["config"]) != (s, c)] + [res]
        doc["provenance"] = provenance(binary, argv)
        doc["provenance"].update({"started_at": started.isoformat(), "ended_at": res["when"],
                                  "load_before": la, "load_after": loadavg()})
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(doc, indent=2, default=str))     # after every pair: a kill loses one pair, not the sweep
        print("   → " + one_line(res))
    print(f"\nwritten: {out}")
    print_table(doc)
    return 0


def one_line(res: dict) -> str:
    b = res["largest_deciding"]; f = res["first_non_deciding"]
    s = []
    if b:
        cone = b["cone_bits"] if b["cone_bits"] is not None else b.get("kept_bits")
        s.append(f"decides to k={b['k']} ({cone} b, {b['verdict']}, {b['engine']}, {b['wall_s']:.1f} s)")
    else:
        s.append("the exact engine decided no probe")
    if f:
        cone = f["cone_bits"] if f["cone_bits"] is not None else f.get("kept_bits")
        s.append(f"first non-decider k={f['k']} ({cone} b): {f['bounded_by']}")
    else:
        s.append("no non-decider within the seeds")
    if res.get("unrunnable"):
        s.append(f"generator limit at k={[u[0] for u in res['unrunnable']]}")
    if res["non_monotone"]:
        s.append("⚠ NON-MONOTONE")
    if res["verdict_mismatches"]:
        s.append(f"⚠ verdict mismatch at k={res['verdict_mismatches']}")
    if res["cone_mismatches"]:
        s.append(f"⚠ cone formula ≠ kept_bits at {res['cone_mismatches']}")
    return "; ".join(s)


# ----------------------------------------------------------------------------- table
def _cone(p: dict | None) -> str:
    if not p:
        return "—"
    gen, kept = p["cone_bits"], p.get("kept_bits")
    if gen is None:
        return f"{kept}" if kept is not None else "?"
    return f"{gen}" if kept in (None, gen) else f"{gen} (kept {kept})"


def _num(v, fmt="{:,}") -> str:
    return fmt.format(v) if v not in (None, 0) else "—"


def print_table(doc: dict) -> None:
    rows = sorted(doc["results"], key=lambda r: (list(SHAPES).index(r["shape"]) if r["shape"] in SHAPES else 99, r["config"]))
    prov = doc.get("provenance", {})
    print(f"\n| shape | config | knob | largest deciding k | cone bits | verdict | engine | wall | iterations | peak nodes | work (BDD ops) | first non-deciding k | cone bits | bounded by |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for r in rows:
        b, f = r["largest_deciding"], r["first_non_deciding"]
        flags = ((" ⚠ non-monotone" if r["non_monotone"] else "") + (" ⚠ verdict mismatch" if r["verdict_mismatches"] else "")
                 + (f" (generator limit at k={[u[0] for u in r['unrunnable']]})" if r.get("unrunnable") else ""))
        knob = "fixed" if SHAPES.get(r["shape"], None) and SHAPES[r["shape"]].fixed else r["knob"].split(":")[0]
        print(f"| {r['shape']} | {r['config']} | {knob} | {b['k'] if b else '—'} | {_cone(b)} | {b['verdict'] if b else '—'} | "
              f"{b['engine'] if b else '—'} | {_num(b['wall_s'], '{:.1f} s') if b else '—'} | {_num(b.get('iterations')) if b else '—'} | "
              f"{_num(b.get('peak_nodes')) if b else '—'} | {_num(b.get('work')) if b else '—'} | "
              f"{f['k'] if f else '—'} | {_cone(f)} | {(f['bounded_by'] if f else '—') + flags} |")
    if prov:
        print(f"\nbinary {prov.get('commit_short')} ({'dirty' if prov.get('git_dirty') == 'yes' else 'clean'}), "
              f"{prov.get('host')} {prov.get('machine')}, started {prov.get('started_at')}, "
              f"load {prov.get('load_before')} → {prov.get('load_after')}; every probe under the shipped defaults "
              f"(MUNUNU_* scrubbed), wall cap per the sweep. Ladder rows (cone > {AUTO_CAP_CEILING}):")
        for r in rows:
            lr = r.get("ladder_row")
            if lr:
                print(f"  {r['shape']} [{r['config']}] k={lr['k']} cone={_cone(lr)} → verdict={lr['verdict']} engine={lr.get('engine')} "
                      f"wall={lr['wall_s']:.1f} s{(' bounded_by=' + lr['bounded_by']) if lr['bounded_by'] else ''}")


# ----------------------------------------------------------------------------- smoke
# The known points, asserted. Each entry: shape, k, config, expectations, wall bound (s).
# Measured 2026-10-14 under the shipped defaults (Intel i7-9750H): the multiplier's exact frontier is
# n = 9 (the 16 M-node arena of the ≤ 40-bit tier); at n = 32 the exact engine abstains `arena-overflow`
# and the cube ladder still decides VIOLATED (~55 s); at n = 48 the ladder gives up too (~40 s). The
# profiling family's "decides to 32 bits" was measured with the arena widened to 1<<26 — not a default.
SMOKE = [
    ("mult", 8, "default", dict(verdict="violated", engine_prefix="exact"), 5.0),
    ("mult", 32, "default", dict(verdict="violated", engine="cube-ladder", exact_abstention="arena-overflow"), 150.0),
    ("mult", 48, "default", dict(verdict="unknown", engine="cube-ladder", exact_abstention="arena-overflow"), 150.0),
    ("i2c", 0, "default", dict(verdict="holds", engine_prefix="exact"), 5.0),
    ("fsm-timeout", 32, "default", dict(verdict="holds", engine_prefix="exact"), 10.0),
    ("fsm-timeout-stuck", 32, "default", dict(verdict="violated", engine_prefix="exact"), 30.0),
]


def smoke(args) -> int:
    binary = resolve_bin(args.bin)
    failures = 0
    print(f"frontier smoke: {len(SMOKE)} probes on {binary}")
    for shape_name, k, config, want, bound in SMOKE:
        p = probe(shape_name, k, config, binary, max(bound, 1.0) * 1.5 if args.cap is None else args.cap)
        problems = []
        if "verdict" in want and p["verdict"] != want["verdict"]:
            problems.append(f"verdict {p['verdict']!r} ≠ {want['verdict']!r}")
        if "engine" in want and p.get("engine") != want["engine"]:
            problems.append(f"engine {p.get('engine')!r} ≠ {want['engine']!r}")
        if "engine_prefix" in want and not (p.get("engine") or "").startswith(want["engine_prefix"]):
            problems.append(f"engine {p.get('engine')!r} does not start with {want['engine_prefix']!r}")
        if "exact_abstention" in want and p.get("exact_abstention") != want["exact_abstention"]:
            problems.append(f"exact_abstention {p.get('exact_abstention')!r} ≠ {want['exact_abstention']!r}")
        if p["timed_out"]:
            problems.append("timed out")
        elif p["wall_s"] > bound:
            problems.append(f"wall {p['wall_s']:.1f} s > {bound:g} s")
        if p.get("engine") is None and p["verdict"] != "error":
            problems.append("no `engine` in the JSON — a release binary older than mununu#682? rebuild with `make build`")
        status = "PASS" if not problems else "FAIL"
        failures += bool(problems)
        print(f"{status}  {shape_name} k={k} [{config}]  verdict={p['verdict']} engine={p.get('engine')} "
              f"exact_abstention={p.get('exact_abstention')} wall={p['wall_s']:.1f}s cone={_cone(p)}"
              + (("\n      " + "; ".join(problems)) if problems else ""))
        if problems and args.verbose:
            print("      stderr tail: " + p["stderr_tail"].replace("\n", "\n      "))
    print(f"\n{len(SMOKE) - failures}/{len(SMOKE)} passed")
    return 1 if failures else 0


# ----------------------------------------------------------------------------- record
def record(args, argv: list[str]) -> int:
    """Archive a sweep's JSON under experiments/<EXP>/ with the manifest the repo's replay gate expects
    (experiments/SCHEMA.md v1). `make replay` does not apply — the replay command is this script."""
    src = Path(args.json)
    if not src.exists():
        sys.exit(f"no sweep JSON at {src}; run `sweep` first")
    exp = args.exp
    exp_dir = REPO / "experiments" / exp
    if not exp_dir.exists():
        parts = exp.split("-", 2)
        if len(parts) != 3 or parts[0] != "EXP":
            sys.exit(f"experiment id must look like EXP-0031-slug, got {exp}")
        subprocess.run([str(REPO / "scripts" / "new_experiment.sh"), parts[1], parts[2]], check=True, cwd=REPO)
    doc = json.loads(src.read_text())
    prov = doc.get("provenance", {})
    shutil.copy(src, exp_dir / "frontier.json")
    hw = exp_dir / "hw-fingerprint.txt"
    hw.write_text(subprocess.run([str(REPO / "scripts" / "capture_hw.sh")], capture_output=True, text=True, cwd=REPO).stdout)
    replay = "scripts/bitcap_frontier.py sweep --fresh   # one shape at a time: --shapes <shape> [--config <c>]"
    (exp_dir / "command.txt").write_text(replay + "\n")
    binary = Path(prov.get("binary", RELEASE_BIN))
    manifest = {
        "schema_version": 1,
        "exp_id": exp,
        "commit": prov.get("commit", git("rev-parse", "HEAD")),
        "branch": prov.get("branch", git("rev-parse", "--abbrev-ref", "HEAD")),
        "git_dirty": prov.get("git_dirty", "unknown"),
        "host": prov.get("host", platform.node()),
        "container": prov.get("container", "no"),
        "command": replay,
        "started_at": prov.get("started_at", ""),
        "ended_at": prov.get("ended_at", ""),
        "exit_code": 0,
        "hw_fingerprint_sha256": sha256_of(hw),
        "criterion_archive_sha256": "",
        "rustc": subprocess.run(["rustc", "--version"], capture_output=True, text=True).stdout.strip() or "unavailable",
        "rust_toolchain_toml_sha256": sha256_of(REPO / "rust-toolchain.toml") if (REPO / "rust-toolchain.toml").exists() else "unavailable",
        "dev_container_dockerfile_sha256": sha256_of(REPO / "docker" / "Dockerfile.dev") if (REPO / "docker" / "Dockerfile.dev").exists() else "unavailable",
        # optional (no schema bump): what this archive actually holds
        "frontier_json_sha256": sha256_of(exp_dir / "frontier.json"),
        "binary_sha256": prov.get("binary_sha256", sha256_of(binary) if binary.exists() else ""),
        "notes": "bit-cap frontier sweep (scripts/bitcap_frontier.py); no Criterion archive — the measurement is frontier.json. "
                 "Wall times are this host's; the deterministic bounds (bit-cap, arena-overflow, iteration-budget, latency) reproduce.",
    }
    (exp_dir / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    draft = exp_dir / ".draft"
    if draft.exists():
        draft.unlink()
    print(f"recorded {exp_dir}: frontier.json, manifest.json, command.txt, hw-fingerprint.txt (.draft removed)")
    print("now fill README.md / log.md from `scripts/bitcap_frontier.py table` and run scripts/check_repro.sh")
    return 0


# ----------------------------------------------------------------------------- cli
def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("sweep", help="bisect the deciding frontier per shape × configuration (merges into the JSON)")
    s.add_argument("--shapes", help=f"comma list; default all: {','.join(SHAPES)}")
    s.add_argument("--config", help=f"one configuration for every shape listed ({', '.join(CONFIGS)}); default: each shape's own")
    s.add_argument("--cap", type=float, default=300.0, help="wall cap per probe, seconds (default 300)")
    s.add_argument("--max-probes", type=int, default=8, help="extra probes per (shape, config) for the bisection and the look past the first non-decider; the seed walk and the ladder row are not budgeted (default 8)")
    s.add_argument("--bin", help=f"binary (default {RELEASE_BIN.relative_to(REPO)}; never the profiling build)")
    s.add_argument("--out", default=str(DEFAULT_OUT), help="sweep JSON, merged by (shape, config)")
    s.add_argument("--fresh", action="store_true", help="start the JSON over instead of merging")
    s.add_argument("--quiet-load", type=float, default=4.0, help="warn when the 1-min load exceeds this (default 4)")

    m = sub.add_parser("smoke", help="the known points, asserted (≈ 1–2 min); exit 1 on any failure")
    m.add_argument("--bin")
    m.add_argument("--cap", type=float, help="override every probe's wall cap")
    m.add_argument("-v", "--verbose", action="store_true", help="print the stderr tail of a failing probe")

    t = sub.add_parser("table", help="print a sweep JSON as a markdown table")
    t.add_argument("json", nargs="?", default=str(DEFAULT_OUT))

    r = sub.add_parser("record", help="archive a sweep JSON under experiments/<EXP>/ with provenance")
    r.add_argument("exp", help="experiment id, e.g. EXP-0031-bitcap-frontier (scaffolded if missing)")
    r.add_argument("--json", default=str(DEFAULT_OUT))

    sub.add_parser("list", help="the shapes, their knobs, seeds and configurations")

    args = ap.parse_args(argv)
    if args.cmd == "sweep":
        return sweep(args, ["scripts/bitcap_frontier.py", *argv])
    if args.cmd == "smoke":
        return smoke(args)
    if args.cmd == "table":
        print_table(json.loads(Path(args.json).read_text()))
        return 0
    if args.cmd == "record":
        return record(args, argv)
    if args.cmd == "list":
        for name, sh in SHAPES.items():
            cones = [sh.cone_of(k) for k in sh.seeds]
            print(f"{name:<18} case={sh.case:<24} knob={sh.knob}\n{'':<18} seeds={list(sh.seeds)}\n{'':<18} cones={cones}  "
                  f"configs={list(sh.configs)}  expected={sh.expected}  ladder_k={sh.ladder_k()}")
        return 0
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
