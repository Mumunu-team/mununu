#!/usr/bin/env python3
"""profile_cases.py — boundary cases for the two μ-engines, calibrated to a CPU-time window, then profiled.

The question this answers: where does the time go on a case that is NON-TRIVIAL (not a 50 ms
control FSM) but still DECIDES (or abstains at a budget, for the abstain-path cases)? Each case
family has one size knob; `calibrate` sweeps it until the run's CPU time (user+sys of the child)
lands inside [--min, --max] seconds — 60..360 by default — and records the sweep. `profile` then
runs the calibrated command under a profiler:

  --tool samply      sampling profiler, native speed, on this host (brew install samply).
                     Output: target/profiles/<case>.json.gz; open with `samply load <file>`
                     (Firefox Profiler: call tree, inverted callee view, flame graph).
  --tool callgrind   valgrind in the `mununu-profile` Docker image; exact instruction counts,
                     20–50× slower. Output: target/profiles/<case>.callgrind.out — open in
                     kcachegrind / qcachegrind (the callee map) — plus a callgrind_annotate text
                     summary. Calibrate a SMALLER instance for this tool (--min 3 --max 15), or
                     pass --toggle-collect to count only inside the fixpoint.

Engines, per the repo's engine-specificity rule:
  exact-*  engine: exact-symbolic — full-state ROBDD (OxiDD), functional next-state substitution,
           bit-blast μ-fixpoint. Driver verb: `btor2 verify-recoverability --target` (exact first).
  cube-*   engine: explicit predicate-cube KMTS — may edges by SMT post-image, must edges by
           SmtHyperMust (Z3 QF_BV ∀∃), CEGAR-refined. Driver verb: `btor2 cegar --engine explicit`.

Subcommands
  list                                   the case catalog, with each family's knob and class
  gen <case> --size K [-o DIR]           write the BTOR2 (and print the command) for one size
  run <case> --size K                    one timed run; prints CPU seconds, verdict, iterations
  calibrate <case> [--min 60 --max 360] [--cap 480] [--seeds a,b,c]
                                         sweep the knob; writes target/profiles/<case>.calibration.json
  profile <case> --size K --tool samply|callgrind [--toggle-collect PAT]
                                         run the calibrated case under the profiler
  all-calibrate [--min --max]            calibrate every family (hours; run in the background)

The binary: --bin (default target/profiling/mununu, falling back to target/release/mununu).
Build the profiling binary with `cargo build --profile profiling -p mununu-cli` — same
optimisation as release plus line tables, so the profile has function names.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import re
import resource
import shlex
import shutil
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PROFILES = REPO / "target" / "profiles"
CASES_DIR = PROFILES / "cases"
WALL_CLASSES = REPO / "crates" / "mununu-core" / "tests" / "fixtures" / "wall_classes"

PEAK_RE = re.compile(r"exact fixpoint: peak (\d+) .*? in (\d+) iteration")
WORK_RE = re.compile(r"work (\d+) BDD ops")
VERDICT_STR_RE = re.compile(r'"verdict":\s*"([a-z_]+)"')
CELLS_RE = re.compile(r'"(true|false|unknown)_cells":\s*(\d+)')
TERM_RE = re.compile(r'"terminated_with":\s*"([A-Za-z]+)"')


def bits(m: int) -> int:
    return max(1, (m - 1).bit_length())


# ----------------------------------------------------------------------------- generators
# Every generator returns (btor2_text, extra_args, env, note). extra_args are appended to the
# driver verb; env is merged into the child's environment.

def gen_exact_raster(v: int):
    """hcount wraps at h=800, vcount advances on that wrap and wraps at v. EF(vcount==v-1) is
    ~h·v iterations deep over a set that compresses (a few nodes per iteration): the
    ITERATION-bound class (the consumer's video_timing, in-repo as probe_553_raster_wrap)."""
    h, hb, vb = 800, 10, bits(v)
    src = (f"1 sort bitvec 1\n2 sort bitvec {hb}\n3 sort bitvec {vb}\n"
           "4 state 2 hcount\n5 state 3 vcount\n6 zero 2\n7 zero 3\n8 init 2 4 6\n9 init 3 5 7\n"
           f"10 constd 2 {h - 1}\n11 constd 3 {v - 1}\n12 eq 1 4 10\n13 eq 1 5 11\n"
           "14 one 2\n15 one 3\n16 add 2 4 14\n17 add 3 5 15\n18 ite 2 12 6 16\n19 next 2 4 18\n"
           "20 ite 3 13 7 17\n21 ite 3 12 20 5\n22 next 3 5 21\n")
    # ~1.3·h·v iterations; the default MUNUNU_BDD_ITER_BUDGET (2^20) would abstain past v≈1000, so the
    # family raises it — that abstention IS the boundary this case sits next to.
    # Measured 2026-10-06: with the iteration budget raised, every size ≥ 4000 plateaus at ~17 s with
    # the peak pinned at 10,027,009 = the fixpoint LATENCY bound (10 M allocated nodes ∧ > 5000 iters),
    # after which the ranking certificate decides. The profiling family therefore disables the two soft
    # bounds and widens the arena so the fixpoint itself runs to convergence; the arena-safety net (80%)
    # remains the only guard. That is what "next to the boundary" means for this engine.
    env = {"MUNUNU_BDD_ITER_BUDGET": str(1 << 28), "MUNUNU_BDD_FIXPOINT_NODES": "0",
           "MUNUNU_BDD_FIXPOINT_ITERS": "0", "MUNUNU_BDD_ARENA_NODES": str(1 << 26)}
    return src, ["--target", f"vcount == {v - 1}"], env, f"raster 800x{v}, ~{int(1.3 * h * v):,} iterations, soft bounds off"


def gen_exact_twocount(m: int):
    """Two counters a, b (each wrapping at m) advance alternately on a free input `turn`; a
    1-bit `done` latches when both read m-1. EF(done==1)'s k-th approximant is the anti-diagonal
    {(a,b): (m-1-a)+(m-1-b) ≤ k}, which does NOT compress: the DEEP-AND-WIDE class (twocount32,
    ~1200 nodes per iteration, the shape that trips the latency bound)."""
    w = bits(m)
    src = (f"1 sort bitvec 1\n2 sort bitvec {w}\n3 input 1 turn\n4 zero 2\n5 state 2 a\n6 state 2 b\n"
           "7 init 2 5 4\n8 init 2 6 4\n9 one 2\n10 add 2 5 9\n11 add 2 6 9\n"
           f"12 constd 2 {m - 1}\n13 eq 1 5 12\n14 eq 1 6 12\n"
           # wrap: a' = (a==m-1) ? 0 : a+1, taken only on turn; b likewise on !turn
           "15 ite 2 13 4 10\n16 ite 2 14 4 11\n17 ite 2 3 15 5\n18 ite 2 -3 16 6\n"
           "19 next 2 5 17\n20 next 2 6 18\n"
           "21 state 1 done\n22 zero 1\n23 init 1 21 22\n24 and 1 13 14\n25 or 1 21 24\n26 next 1 21 25\n")
    env = {"MUNUNU_BDD_ITER_BUDGET": str(1 << 28), "MUNUNU_BDD_FIXPOINT_NODES": "0",
           "MUNUNU_BDD_FIXPOINT_ITERS": "0", "MUNUNU_BDD_ARENA_NODES": str(1 << 26)}
    return src, ["--target", "done == 1"], env, f"twocount m={m} (w={w}), ~{2 * (m - 1):,} iterations, soft bounds off"


def gen_exact_forward_twocount(m: int):
    """The twocount32 shape, FORWARD: bad = (a == m-1 ∧ b == m-1) with two counters advancing
    alternately on a free input. `btor2 verify --owned-only` runs the exact member's bad-reachability
    (forward image from init: the k-th layer is the anti-diagonal a+b=k, ~1200 nodes per step, the
    set that does NOT compress) beside native BMC / interp / deep-CEX on detached threads. Read the
    exact member in `reachable_by`/`unreachable_by`; past m≈2^13 it abstains on the LATENCY bound
    (twocount32: ~280 s to step 8190). The profile is per-thread; look at the exact engine's thread."""
    w = bits(m)
    src = (f"1 sort bitvec 1\n2 sort bitvec {w}\n3 input 1 turn\n4 zero 2\n5 state 2 a\n6 state 2 b\n"
           "7 init 2 5 4\n8 init 2 6 4\n9 one 2\n10 add 2 5 9\n11 add 2 6 9\n12 ite 2 3 5 10\n13 ite 2 -3 6 11\n"
           f"14 next 2 5 12\n15 next 2 6 13\n16 constd 2 {m - 1}\n17 eq 1 5 16\n18 eq 1 6 16\n19 and 1 17 18\n20 bad 19\n")
    return src, ["--owned-only", "--owned-timeout-ms", "900000"], {}, f"forward twocount m={m} (w={w}), bad at (m-1,m-1), owned-only portfolio"


def gen_exact_relational(n: int):
    """Two held n-bit registers with free initial values and a `done` latch on a==b. One
    pre-image: {a==b} — Θ(2^n) nodes under cell-major order, O(n) interleaved. The
    REPRESENTATION-bound class; runs with MUNUNU_BDD_VAR_ORDER=cell-major to exhibit it. Expect a
    cliff: seconds, then an arena abstention — the calibrator reports whichever it finds."""
    src = (f"1 sort bitvec 1\n2 sort bitvec {n}\n3 state 2 a\n4 state 2 b\n5 next 2 3 3\n6 next 2 4 4\n"
           "7 eq 1 3 4\n8 state 1 done\n9 zero 1\n10 init 1 8 9\n11 or 1 8 7\n12 next 1 8 11\n")
    # 2026-10-06: at n=11 under the DEFAULT 2^24 arena this returned a definite HOLDS where 2^23 and
    # 2^26 returned VIOLATED. Diagnosed and fixed the same week (#603: the cube paths pinned a
    # free-init register to zero in their reset cube; the exact engine's arena abstention handed the
    # case to that path — docs/consumer-briefings/2026-10-free-init-reset-cube.md). The 2^26 pin is
    # KEPT for a different reason: it keeps the case on the exact engine at every n of the sweep, so
    # the family measures the representation-bound fixpoint and not the hand-off.
    env = {"MUNUNU_BDD_VAR_ORDER": "cell-major", "MUNUNU_BDD_ARENA_NODES": str(1 << 26)}
    return src, ["--target", "done == 1"], env, f"relational a==b, n={n} bits, cell-major order, arena 2^26"


def gen_exact_mult(n: int):
    """`done` latches on a*b == k over two free n-bit registers. A multiplier's BDD is exponential
    under every order (Bryant 1986): the ABSTAIN-PATH case — the exact engine bit-blasts the
    schoolbook product and abstains on the node budget. Profiles the build phase, not a fixpoint."""
    k = (1 << (2 * n - 3)) + 1
    src = (f"1 sort bitvec 1\n2 sort bitvec {n}\n3 sort bitvec {2 * n}\n3 state 2 a\n4 state 2 b\n"
           "5 next 2 3 3\n6 next 2 4 4\n7 uext 3 3 {n}\n8 uext 3 4 {n}\n9 mul 3 7 8\n"
           f"10 constd 3 {k}\n11 eq 1 9 10\n12 state 1 done\n13 zero 1\n14 init 1 12 13\n15 or 1 12 11\n16 next 1 12 15\n")
    src = src.replace("3 sort bitvec", "17 sort bitvec", 1).replace("7 uext 3 3", "7 uext 17 3").replace("8 uext 3 4", "8 uext 17 4") \
             .replace("9 mul 3 7 8", "9 mul 17 7 8").replace("10 constd 3", "10 constd 17").replace("{n}", str(n))
    return src, ["--target", "done == 1"], {}, f"multiplier a*b==k, n={n} bits per operand (abstain path)"


def gen_cube_synth_cells(n: int, w: int = 32):
    """SYNTHETIC CONTROL: `ctrl` returns to 0 when a width-w datapath (barrel-shifted by a free
    input) hits any of n constants; predicates = ctrl==0 plus the n equalities → 2^(n+1) cells. On
    a design this small Z3 answers each query in microseconds at ANY width, so the cost is query
    count × fixed per-query overhead: ~2 s at the 1024-cell cap (n=9). It cannot reach the window —
    it is here as the contrast to the real-RTL case, where the same cell count costs minutes."""
    L = ["1 sort bitvec 1", f"2 sort bitvec {w}", "3 state 1 ctrl", "4 state 2 data", "5 one 1", "6 zero 2", "7 one 2",
         "8 init 1 3 5", "9 init 2 4 6", "12 input 2 inp", "13 or 2 12 7",
         "14 sll 2 4 13", "15 srl 2 4 7", "16 xor 2 14 15", "10 add 2 16 7", "11 next 2 4 10"]
    nid, eqs = 20, []
    for k in range(n):
        L.append(f"{nid} constd 2 {3 * k + 1}"); cn = nid; nid += 1
        L.append(f"{nid} eq 1 4 {cn}"); eqs.append(nid); nid += 1
    acc = eqs[0]
    for e in eqs[1:]:
        L.append(f"{nid} or 1 {acc} {e}"); acc = nid; nid += 1
    L.append(f"{nid} not 1 {acc}"); nothit = nid; nid += 1
    L.append(f"{nid} and 1 3 {nothit}"); nxt = nid; nid += 1
    L.append(f"{nid} next 1 3 {nxt}")
    args = ["--formula", "nu Z.((mu Y.((ctrl == 0) || <>Y)) && []Z)", "--predicate", "ctrl == 0:ctrl=0"]
    for k in range(n):
        c = 3 * k + 1
        args += ["--predicate", f"data == {c}:data={c}"]
    return "\n".join(L) + "\n", args, {}, f"synthetic cube n={n} predicates (2^{n + 1} cells), w={w}"


# Control predicates on the i2c lift, in the order they open up the control web. The cube's
# per-predicate cost on this design was measured 2026-10-06: |P| = 2/4/6/8/9 → 0.2/0.6/2.9/23/50 s.
I2C_PREDS = [
    ("byte_controller.bit_controller.c_state", 1), ("byte_controller.c_state", 0),
    ("byte_controller.bit_controller.cmd", 0), ("tip", 1), ("byte_controller.bit_controller.clk_en", 1),
    ("byte_controller.go", 1), ("byte_controller.bit_controller.cmd_stop", 1),
    ("byte_controller.bit_controller.slave_wait", 1), ("byte_controller.bit_controller.sda_chk", 1),
    ("byte_controller.ld", 1), ("byte_controller.shift", 1), ("byte_controller.core_ack", 1),
]


def gen_cube_rtl_i2c_preds(k: int):
    """REAL RTL: the i2c master lift (44 registers, 154 state bits, 10 free inputs), the
    register-dominated class. One cube lift with the first k control predicates from I2C_PREDS:
    2^k cells, each needing a post-image may query and a hyper-must ∀∃ query over the design's
    freed-input cone — the structure that grinds, not arithmetic width. |P| ≤ 10 under the
    1024-cell cap. ⚠ The cube's verdict here is NOT to be read as a result: the matrix classes
    this target as ⊥ and the exact engine is the oracle; the object of study is the lift cost."""
    src = (WALL_CLASSES / "i2c_scl_padoen.btor").read_text()
    args = ["--formula", "nu Z.((mu Y.((scl_padoen_o == 1) || <>Y)) && []Z)"]
    for name, val in I2C_PREDS[:k]:
        args += ["--predicate", f"{name} == {val}:{name}={val}"]
    return src, args, {}, f"i2c lift, |P|={k} control predicates ({2 ** k} cells), one lift, no refinement"


def gen_cube_rtl_i2c_cegar(iters: int, k: int = 8):
    """Same lift with |P|=8 seeded and `--max-iterations iters`. ⚠ The knob is INERT on this
    design (found 2026-10-08, Roadmap 3 item 3): the verdict is a definite VIOLATED at the first
    lift, so no refinement ever runs and the 1/2/3-iteration calibration rows (20.5 / 18.6 /
    18.6 s at fed081c) were one lift each, not a re-lift cost. `cube-synth-cells` does not refine
    either (its WP source is exhausted at iteration 1). The no-lemma-cache cost (cegar.rs:48)
    the roadmap's stage C3 targets has NO case in this catalogue; it needs a design whose first
    lift lands on ⊥ with a refinable predicate source — the prerequisite before C3 is measured.
    Its upper bound is (rounds − 1) × one lift, which after #616/#617 is 0.1–0.7 s on this cone."""
    src, args, env, _ = gen_cube_rtl_i2c_preds(k)
    return src, args, env, f"i2c lift, |P|={k} + {iters} CEGAR iteration(s)"


def gen_exact_rtl(name: str, target: str):
    def g(_size: int):
        return (WALL_CLASSES / name).read_text(), ["--target", target], {}, f"RTL {name}, fixed size"
    return g


CASES: dict[str, dict] = {
    # name: generator, driver verb, knob name, default seeds, class, engine
    "exact-raster":     dict(gen=gen_exact_raster, verb="verify-recoverability", knob="v (raster lines, h=800)",
                             seeds=[4000, 8000, 16000, 32000, 64000], cls="iteration-bound, compressing", engine="exact-symbolic"),
    "exact-twocount":   dict(gen=gen_exact_twocount, verb="verify-recoverability", knob="m (wrap modulus of each counter)",
                             seeds=[524288, 2097152, 8388608, 33554432], cls="deep, backward EF with a done-latch", engine="exact-symbolic"),
    "exact-forward":    dict(gen=gen_exact_forward_twocount, verb="verify", knob="m (counter modulus; bad at (m-1,m-1))",
                             seeds=[4096, 8192, 16384, 65536, 4294967296], cls="forward reach, non-compressing anti-diagonal (twocount32)", engine="exact-symbolic (+ owned portfolio threads)"),
    "exact-relational": dict(gen=gen_exact_relational, verb="verify-recoverability", knob="n (bits per register)",
                             seeds=[11, 12, 13], cls="representation-bound (cell-major), VIOLATED + witness path", engine="exact-symbolic"),
    "exact-mult":       dict(gen=gen_exact_mult, verb="verify-recoverability", knob="n (bits per operand)",
                             seeds=[32, 40, 48, 64], cls="multiplier bit-blast; decides VIOLATED up to 32 bits, then the node budget", engine="exact-symbolic"),
    "exact-rtl-i2c":    dict(gen=gen_exact_rtl("i2c_scl_padoen.btor", "scl_padoen_o == 1"), verb="verify-recoverability",
                             knob="(fixed)", seeds=[0], cls="reference: RTL, sub-second, below the window", engine="exact-symbolic"),
    "exact-rtl-aes":    dict(gen=gen_exact_rtl("aes_cipher_control_fsm.btor", "aes_cipher_ctrl_cs == 9"), verb="verify-recoverability",
                             knob="(fixed)", seeds=[0], cls="reference: RTL control FSM, sub-second, below the window", engine="exact-symbolic"),
    "cube-rtl-i2c":     dict(gen=gen_cube_rtl_i2c_preds, verb="cegar", knob="|P| (control predicates, ≤10)",
                             seeds=[6, 8, 9, 10], cls="RTL register-dominated, hyper-must over a freed-input cone", engine="explicit predicate-cube"),
    "cube-rtl-i2c-cegar": dict(gen=gen_cube_rtl_i2c_cegar, verb="cegar", knob="CEGAR iterations (|P|=8 seeded)",
                             seeds=[1, 2, 3, 4], cls="RTL, refinement loop re-lift cost", engine="explicit predicate-cube"),
    "cube-synth-cells": dict(gen=gen_cube_synth_cells, verb="cegar", knob="n (predicates, 2^(n+1) cells, ≤9)",
                             seeds=[6, 8, 9], cls="synthetic control: cheap by construction", engine="explicit predicate-cube"),
}

CUBE_FLAGS = ["--engine", "explicit", "--must-edge-inference", "smt-hyper-must", "--json"]


# ----------------------------------------------------------------------------- building a run
def find_bin(explicit: str | None) -> Path:
    for p in ([Path(explicit)] if explicit else []) + [REPO / "target" / "profiling" / "mununu", REPO / "target" / "release" / "mununu"]:
        if p.exists():
            return p
    sys.exit("no mununu binary: cargo build --profile profiling -p mununu-cli")


def materialise(case: str, size: int) -> tuple[Path, list[str], dict, str]:
    spec = CASES[case]
    src, args, env, note = spec["gen"](size)
    CASES_DIR.mkdir(parents=True, exist_ok=True)
    path = CASES_DIR / f"{case}-{size}.btor2"
    path.write_text(src)
    return path, args, env, note


def command(case: str, size: int, binary: Path) -> tuple[list[str], dict, str]:
    spec = CASES[case]
    path, args, env, note = materialise(case, size)
    cmd = [str(binary), "--quiet", "btor2", spec["verb"], str(path), *args]
    if spec["verb"] == "cegar":
        iters = size if case == "cube-rtl-i2c-cegar" else 0
        cmd += [*CUBE_FLAGS, "--max-iterations", str(iters)]
    return cmd, env, note


def run_timed(cmd: list[str], env: dict, cap_s: float) -> dict:
    full_env = {**os.environ, "MUNUNU_BDD_REPORT_WORK": "1", **env}
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
    t0 = time.perf_counter()
    timed_out = False
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, env=full_env, timeout=cap_s)
        out, err, code = proc.stdout, proc.stderr, proc.returncode
    except subprocess.TimeoutExpired as e:
        timed_out, code = True, None
        out = (e.stdout or b"").decode() if isinstance(e.stdout, bytes) else (e.stdout or "")
        err = (e.stderr or b"").decode() if isinstance(e.stderr, bytes) else (e.stderr or "")
    wall = time.perf_counter() - t0
    r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)
    iters, peak, work = 0, 0, 0
    for m in PEAK_RE.finditer(err):
        iters += int(m.group(2)); peak = max(peak, int(m.group(1)))
    for m in WORK_RE.finditer(err):
        work += int(m.group(1))
    verdict = "TIMEOUT" if timed_out else (VERDICT_STR_RE.search(out).group(1) if VERDICT_STR_RE.search(out) else None)
    if verdict and '"reachable_by"' in out:
        rb = re.search(r'"reachable_by":\s*\[([^\]]*)\]', out); ub = re.search(r'"unreachable_by":\s*\[([^\]]*)\]', out)
        members = (rb.group(1) if rb else "") + (ub.group(1) if ub else "")
        verdict += " exact-decided" if '"exact"' in members else " exact-abstained"
    if verdict is None and not timed_out:
        cells = {k: int(v) for k, v in CELLS_RE.findall(out)}
        term = TERM_RE.search(out)
        if cells:
            verdict = f"cells T{cells.get('true', 0)}/F{cells.get('false', 0)}/U{cells.get('unknown', 0)}" + (f" {term.group(1)}" if term else "")
        else:
            verdict = f"exit {code}"
    abstain = re.search(r"abstained on the ([A-Z\- ]+?) (bound|budget|CAP)", err)
    return {"cpu_s": cpu, "wall_s": wall, "exit": code, "timed_out": timed_out, "verdict": verdict,
            "iterations": iters or None, "peak_nodes": peak or None, "work": work or None,
            "abstained_on": abstain.group(0) if abstain else None,
            "stderr_tail": err[-600:]}


def fmt_run(size, r: dict) -> str:
    extra = f"  iters={r['iterations']:,}" if r["iterations"] else ""
    extra += f"  peak={r['peak_nodes']:,}" if r["peak_nodes"] else ""
    extra += f"  work={r['work']:,}" if r.get("work") else ""
    extra += f"  [{r['abstained_on']}]" if r["abstained_on"] else ""
    return f"size={size:<12} cpu={r['cpu_s']:8.1f}s  wall={r['wall_s']:8.1f}s  verdict={r['verdict']}{extra}"


# ----------------------------------------------------------------------------- calibrate
def calibrate(case: str, binary: Path, lo: float, hi: float, cap: float, seeds: list[int], max_bisect: int = 6) -> dict:
    spec = CASES[case]
    print(f"\n== calibrate {case}  engine: {spec['engine']}  knob: {spec['knob']}  window [{lo}, {hi}] s CPU, cap {cap} s")
    sweep: list[tuple[int, dict]] = []
    under, over = None, None  # (size, cpu)
    hit = None

    def probe(size: int) -> dict:
        cmd, env, note = command(case, size, binary)
        r = run_timed(cmd, env, cap)
        sweep.append((size, r))
        print("   " + fmt_run(size, r), flush=True)
        return r

    for s in seeds:
        r = probe(s)
        c = r["cpu_s"] if not r["timed_out"] else cap
        if lo <= c <= hi and not r["timed_out"]:
            hit = (s, r); break
        if c < lo:
            under = (s, c)
        else:
            over = (s, c); break
    # Bisect in log space between the last under and the first over.
    n = 0
    while hit is None and under and over and n < max_bisect and over[0] - under[0] > 1:
        n += 1
        # geometric midpoint, nudged by the measured slope when both points are real
        mid = int(round((under[0] * over[0]) ** 0.5))
        if mid in (under[0], over[0]):
            break
        r = probe(mid)
        c = r["cpu_s"] if not r["timed_out"] else cap
        if lo <= c <= hi and not r["timed_out"]:
            hit = (mid, r)
        elif c < lo:
            under = (mid, c)
        else:
            over = (mid, c)
    PROFILES.mkdir(parents=True, exist_ok=True)
    result = {
        "case": case, "engine": spec["engine"], "class": spec["cls"], "knob": spec["knob"],
        "window_cpu_s": [lo, hi], "cap_s": cap,
        "binary": str(binary), "commit": subprocess.run(["git", "-C", str(REPO), "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip(),
        "host": os.uname().nodename, "when": _dt.datetime.now(_dt.timezone.utc).isoformat(),
        "calibrated_size": hit[0] if hit else None,
        "calibrated_run": hit[1] if hit else None,
        "sweep": [{"size": s, **{k: v for k, v in r.items() if k != "stderr_tail"}} for s, r in sweep],
        "command": command(case, hit[0], binary)[0] if hit else None,
        "env": command(case, hit[0], binary)[1] if hit else None,
    }
    (PROFILES / f"{case}.calibration.json").write_text(json.dumps(result, indent=2))
    if hit:
        print(f"   → calibrated: size={hit[0]} at {hit[1]['cpu_s']:.1f} s CPU  ({PROFILES / (case + '.calibration.json')})")
    else:
        print(f"   → no size landed in the window (see the sweep in {PROFILES / (case + '.calibration.json')})")
    return result


# ----------------------------------------------------------------------------- profile
def profile(case: str, size: int, tool: str, binary: Path, toggle: str | None, image: str) -> int:
    PROFILES.mkdir(parents=True, exist_ok=True)
    cmd, env, note = command(case, size, binary)
    stem = PROFILES / f"{case}-{size}"
    print(f"== profile {case} size={size} with {tool}: {note}")
    if tool == "samply":
        if not shutil.which("samply"):
            sys.exit("samply not installed: brew install samply")
        out = stem.with_suffix(".samply.json.gz")
        full = ["samply", "record", "--save-only", "-o", str(out), "--", *cmd]
        print("   " + shlex.join(full))
        rc = subprocess.run(full, env={**os.environ, "MUNUNU_BDD_REPORT_WORK": "1", **env}).returncode
        print(f"   → {out}\n   open with:  samply load {out}   (serves the Firefox Profiler on localhost; "
              f"use the Call Tree → 'Invert call stack' for the callee view, or the Flame Graph tab)")
        if rc == 0 or out.exists():
            summarize_samply(out, 20)
        return rc
    if tool == "callgrind":
        # Paths inside the container: the repo at /work, the cargo target volume at /ct.
        rel_bin = "/ct/profiling/mununu"
        rel_case = "/work/" + str(Path(cmd[3 + 1]).resolve().relative_to(REPO))
        in_cmd = [rel_bin, *cmd[1:4], rel_case, *cmd[5:]]
        out = f"/work/target/profiles/{case}-{size}.callgrind.out"
        vg = ["valgrind", "--tool=callgrind", f"--callgrind-out-file={out}", "--dump-instr=no", "--collect-jumps=no"]
        if toggle:
            vg += ["--collect-atstart=no", f"--toggle-collect={toggle}"]
        envflags = [f"-e{k}={v}" for k, v in {"MUNUNU_BDD_REPORT_WORK": "1", **env}.items()]
        dock = ["docker", "run", "--rm", "-v", f"{REPO}:/work", "-v", "mununu-target:/ct", "-w", "/work", *envflags, image]
        full = [*dock, *vg, *in_cmd]
        print("   " + shlex.join(full))
        rc = subprocess.run(full).returncode
        host_out = stem.with_suffix(".callgrind.out")
        ann = stem.with_suffix(".callgrind.txt")
        with ann.open("w") as f:
            subprocess.run([*dock, "callgrind_annotate", "--inclusive=yes", "--threshold=99", out], stdout=f)
        print(f"   → {host_out}  (open in kcachegrind / qcachegrind: the Callee Map tab)\n   → {ann}  (inclusive cost per function, text)")
        print("   note: callgrind_annotate over-counts INCLUSIVE cost on recursive functions (rows above 100%);"
              " kcachegrind's cycle handling and the SELF column below are exact")
        with ann.with_suffix(".self.txt").open("w") as f:
            subprocess.run([*dock, "callgrind_annotate", "--inclusive=no", "--threshold=95", out], stdout=f)
        print("   top SELF cost:")
        for line in ann.with_suffix(".self.txt").read_text().splitlines()[:40]:
            if re.match(r"^\s*[\d,]+ \(", line):
                print("     " + re.sub(r"/usr/local/cargo/registry/src/[^/]+/", "", line)[:150])
        print("   top inclusive functions:")
        for line in ann.read_text().splitlines()[:60]:
            if re.match(r"^\s*[\d,]+ \(", line):
                print("     " + line[:150])
        return rc
    sys.exit(f"unknown tool {tool}")



# ----------------------------------------------------------------------------- summarize (samply)
_SYMTAB_CACHE: dict[str, tuple[list[int], list[str]]] = {}


def _symtab(binary: str) -> tuple[list[int], list[str]]:
    """Sorted (relative address, demangled name) from `nm -n --demangle`; Mach-O __TEXT base
    subtracted so addresses match samply's lib-relative frame addresses. Hash suffixes stripped."""
    if binary in _SYMTAB_CACHE:
        return _SYMTAB_CACHE[binary]
    base = 0
    if sys.platform == "darwin":
        ot = subprocess.run(["otool", "-l", binary], capture_output=True, text=True).stdout
        m = re.search(r"segname __TEXT\n\s+vmaddr 0x([0-9a-f]+)", ot)
        base = int(m.group(1), 16) if m else 0
    addrs, names = [], []
    nm = subprocess.run(["nm", "-n", "--demangle", binary], capture_output=True, text=True).stdout
    for line in nm.splitlines():
        parts = line.split(" ", 2)
        if len(parts) < 3 or parts[1] not in ("t", "T"):
            continue
        try:
            a = int(parts[0], 16) - base
        except ValueError:
            continue
        name = re.sub(r"::h[0-9a-f]{16}$", "", parts[2].strip())
        # LLVM's per-build suffixes (`(.llvm.NNN)`, `.llvm.NNN`) differ between binaries and would
        # make the same function look like two in a before/after comparison.
        name = re.sub(r"\s*\(\.llvm\.\d+\)|\.llvm\.\d+", "", name)
        for a_, b_ in (("$LT$", "<"), ("$GT$", ">"), ("$C$", ","), ("$u20$", " "), ("$u27$", "'"), ("$u5b$", "["), ("$u5d$", "]"), ("$RF$", "&"), ("..", "::")):
            name = name.replace(a_, b_)
        addrs.append(a); names.append(name)
    _SYMTAB_CACHE[binary] = (addrs, names)
    return addrs, names


def _resolve(binary: str, addr: int) -> str:
    import bisect
    addrs, names = _symtab(binary)
    i = bisect.bisect_right(addrs, addr) - 1
    return names[i] if i >= 0 else f"0x{addr:x}"


def samply_tables(path: Path) -> dict[str, dict]:
    """Per thread: sample count plus self and inclusive sample counts per function, from a saved
    samply profile. Frames are symbolised through `nm` on the recorded binary (samply itself
    symbolises only in its UI); monomorphised copies of one function are merged by name."""
    import gzip
    prof = json.load(gzip.open(path))
    out: dict[str, dict] = {}
    for th in prof["threads"]:
        n = th["samples"]["length"]
        if n < 10:
            continue
        strings, funcs, frames, stacks = th["stringArray"], th["funcTable"], th["frameTable"], th["stackTable"]
        libs = prof["libs"]
        res_lib = th["resourceTable"].get("lib") or []
        func_lib = [res_lib[r] if (r is not None and r < len(res_lib)) else None for r in funcs["resource"]]
        fname = []
        for fi, si in enumerate(funcs["name"]):
            raw = strings[si]
            li = func_lib[fi]
            lpath = libs[li]["path"] if li is not None and li < len(libs) else None
            if raw.startswith("0x") and lpath and os.path.exists(lpath) and shutil.which("nm"):
                fname.append(_resolve(lpath, int(raw, 16)) + ("" if "/usr/lib" not in lpath else "  [" + os.path.basename(lpath) + "]"))
            else:
                fname.append(raw)
        self_c: dict[str, int] = {}
        incl_c: dict[str, int] = {}
        weights = th["samples"].get("weight") or [1] * n
        memo: dict[int, list[int]] = {}

        def chain(si: int) -> list[int]:  # func indices root→leaf, memoised per stack id
            if si in memo:
                return memo[si]
            pre = stacks["prefix"][si]
            base = chain(pre) if pre is not None else []
            res = base + [frames["func"][stacks["frame"][si]]]
            memo[si] = res
            return res

        total = 0
        for si, w in zip(th["samples"]["stack"], weights):
            if si is None:
                continue
            w = w or 1
            total += w
            fs = [fname[f] for f in chain(si)]
            self_c[fs[-1]] = self_c.get(fs[-1], 0) + w
            for f in set(fs):
                incl_c[f] = incl_c.get(f, 0) + w
        name = th["name"]
        key = name if name not in out else f"{name}#{len(out)}"
        out[key] = {"samples": total, "self": self_c, "incl": incl_c}
    return out


def summarize_samply(path: Path, top: int = 25, thread_filter: str | None = None) -> None:
    """Print self and inclusive time per function — the callee map in text form, no browser."""
    path = Path(path)
    for name, t in samply_tables(path).items():
        if thread_filter and thread_filter not in name:
            continue
        total = t["samples"]
        print(f"\nthread `{name}`  samples={total}  ({path.name})")
        print(f"  {'self%':>6} {'incl%':>6}  function (top {top} by SELF time)")
        for f, c in sorted(t["self"].items(), key=lambda kv: -kv[1])[:top]:
            print(f"  {100 * c / total:6.1f} {100 * t['incl'].get(f, 0) / total:6.1f}  {f[:120]}")
        print(f"  {'self%':>6} {'incl%':>6}  function (top {top} by INCLUSIVE time — the callee map)")
        for f, c in sorted(t["incl"].items(), key=lambda kv: -kv[1])[:top]:
            print(f"  {100 * t['self'].get(f, 0) / total:6.1f} {100 * c / total:6.1f}  {f[:120]}")


# ----------------------------------------------------------------------------- cli
def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(prog="profile_cases.py", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--bin", default=None, help="mununu binary (default target/profiling/mununu, else target/release/mununu)")
    sub = p.add_subparsers(dest="sub", required=True)
    sub.add_parser("list")
    g = sub.add_parser("gen"); g.add_argument("case", choices=CASES); g.add_argument("--size", type=int, required=True); g.add_argument("-o", type=Path)
    r = sub.add_parser("run"); r.add_argument("case", choices=CASES); r.add_argument("--size", type=int, required=True); r.add_argument("--cap", type=float, default=900)
    c = sub.add_parser("calibrate"); c.add_argument("case", choices=CASES)
    for q in (c,):
        q.add_argument("--min", type=float, default=60); q.add_argument("--max", type=float, default=360)
        q.add_argument("--cap", type=float, default=480, help="kill a probe past this many WALL seconds")
        q.add_argument("--seeds", default=None, help="comma-separated knob values to try in order")
    a = sub.add_parser("all-calibrate"); a.add_argument("--min", type=float, default=60); a.add_argument("--max", type=float, default=360); a.add_argument("--cap", type=float, default=480)
    a.add_argument("--only", default=None, help="comma-separated case names")
    pr = sub.add_parser("profile"); pr.add_argument("case", choices=CASES); pr.add_argument("--size", type=int, required=True)
    pr.add_argument("--tool", choices=("samply", "callgrind"), default="samply")
    pr.add_argument("--toggle-collect", default=None, help="callgrind: count only inside functions matching this glob, e.g. '*ExactModel*'")
    pr.add_argument("--image", default="mununu-profile")
    sm = sub.add_parser("summarize", help="self/inclusive time per function from a samply profile")
    sm.add_argument("profile", type=Path); sm.add_argument("--top", type=int, default=25); sm.add_argument("--thread", default=None)
    args = p.parse_args(argv)

    if args.sub == "summarize":
        summarize_samply(args.profile, args.top, args.thread)
        return 0
    if args.sub == "list":
        print(f"{'case':<18} {'engine':<26} {'knob':<40} class")
        for k, s in CASES.items():
            print(f"{k:<18} {s['engine']:<26} {s['knob']:<40} {s['cls']}")
        return 0
    binary = find_bin(args.bin)
    if args.sub == "gen":
        path, a_, env, note = materialise(args.case, args.size)
        if args.o:
            args.o.parent.mkdir(parents=True, exist_ok=True); shutil.copy(path, args.o); path = args.o
        cmd, env, note = command(args.case, args.size, binary)
        print(f"{path}\n{note}\n{' '.join(f'{k}={v}' for k, v in env.items())} {shlex.join(cmd)}".strip())
        return 0
    if args.sub == "run":
        cmd, env, note = command(args.case, args.size, binary)
        print(note); print("   " + shlex.join(cmd))
        res = run_timed(cmd, env, args.cap)
        print("   " + fmt_run(args.size, res))
        # `--fail-on violated` is the verb's default, so exit 1 on a VIOLATED verdict is expected.
        if res["verdict"] is None or str(res["verdict"]).startswith("exit"):
            print(res["stderr_tail"])
        return 0
    if args.sub == "calibrate":
        seeds = [int(x) for x in args.seeds.split(",")] if args.seeds else CASES[args.case]["seeds"]
        calibrate(args.case, binary, args.min, args.max, args.cap, seeds)
        return 0
    if args.sub == "all-calibrate":
        names = args.only.split(",") if args.only else list(CASES)
        for k in names:
            calibrate(k, binary, args.min, args.max, args.cap, CASES[k]["seeds"])
        return 0
    if args.sub == "profile":
        return profile(args.case, args.size, args.tool, binary, args.toggle_collect, args.image)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
