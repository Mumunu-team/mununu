# Scripts

## Dependency Auditing

### `audit-dependencies.sh`

Comprehensive dependency auditing script that checks for:
- **Security vulnerabilities** using `cargo-audit` (RustSec advisory database)
- **Outdated dependencies** using `cargo-outdated`

**Usage:**
```bash
./scripts/audit-dependencies.sh
```

**Prerequisites:**
```bash
# Install audit tools (one-time setup)
cargo install cargo-audit --locked
cargo install cargo-outdated
```

**What it does:**
1. Checks if `cargo-audit` is installed and runs security vulnerability scan
2. Checks if `cargo-outdated` is installed and reports outdated dependencies
3. Provides helpful error messages and installation instructions if tools are missing

**Integration:**
- This script is automatically run in CI (GitHub Actions)
- Security vulnerabilities will fail the CI build
- Outdated dependencies are reported but don't fail the build

## generate_bpmn_examples

Generates a markdown file containing BPMN XML examples from tests and their corresponding CLTS DSL translations.

### Usage

```bash
# Generate default output file (bpmn_examples_output.md)
cargo run --bin generate_bpmn_examples

# Generate to a custom file
cargo run --bin generate_bpmn_examples output.md
```

### Output Format

The script generates a markdown file with:

1. **BPMN XML** - The original XML content from test examples
2. **CLTS DSL Translation** - The translated CLTS DSL context
3. **JSON Output** - JSON representation containing:
   - `context_name`: Name of the generated context
   - `dsl_source`: The full CLTS DSL source code
   - `automata_count`: Number of automata in the context
   - `sidecars_count`: Number of sidecar documents

### Example Output

```markdown
## Example 1: test_parse_minimal_bpmn_xml

### BPMN XML

```xml
<?xml version="1.0" encoding="UTF-8"?>
...
```

### CLTS DSL Translation

```clts
context SimpleProcess {
    ...
}
```

### JSON Output

```json
{
  "context_name": "SimpleProcess",
  "dsl_source": "context SimpleProcess {...}",
  "automata_count": 1,
  "sidecars_count": 0
}
```
```


## ab_time.py — before/after timing with noise control

> Source of truth: [`scripts/ab_time.py`](ab_time.py) — surface: CLI-only — a measurement tool for engine work, not a user-facing verification capability.

Criterion covers `cargo bench` targets. `ab_time.py` covers everything you time by hand — a CLI
verb, an `#[ignore]`d probe, a test filter. It repeats the command, keeps every sample, reports
mean ± standard deviation, median and coefficient of variation, and decides whether two records
differ by more than noise (Welch t-test, Mann–Whitney cross-check, a change threshold). It also
parses the exact engine's `peak … in K iteration(s) … work W BDD ops` line so the host-independent
iteration count and BDD-op count are compared before the wall clock. Turn that line on with
`MUNUNU_BDD_REPORT_WORK=1`, not `MUNUNU_BDD_REPORT_PEAK=1`: the peak report runs a collection to
read the residual, and that sweep is 16–25% of self time on the corpus — it would be timed too.

```bash
scripts/ab_time.py preflight                       # load, heavy apps, power, thermal, running builds
MUNUNU_BDD_REPORT_WORK=1 scripts/ab_time.py run before -n 10 --caffeinate -- \
    target/release/mununu --quiet btor2 verify-recoverability d.btor2 --target "s == 0"
# change, rebuild, then:
MUNUNU_BDD_REPORT_WORK=1 scripts/ab_time.py run after  -n 10 --caffeinate -- <same command>
scripts/ab_time.py compare target/ab_time/before.json target/ab_time/after.json
# strongest form, two binaries interleaved A B A B … so drift hits both equally:
scripts/ab_time.py ab -n 10 --a '/tmp/mununu-before …' --b '/tmp/mununu-after …'
```

Records land in `target/ab_time/<label>.json` (git-ignored) with per-run samples, stdout/stderr
files, git commit and dirty flag, the `capture_hw.sh` fingerprint hash and the load average
before and after. Verdicts: `FASTER`/`SLOWER` beyond the threshold at the chosen alpha,
`NO SIGNIFICANT CHANGE`, or `TOO NOISY` when either side's cv exceeds 10% — in which case quiet the
machine (the preflight lists what to close), raise `-n`, or use `ab`.

## profile_cases.py — boundary cases for the two μ-engines, calibrated and profiled

> Source of truth: [`scripts/profile_cases.py`](profile_cases.py) + [`docker/Dockerfile.profile`](../docker/Dockerfile.profile) — surface: CLI-only — an engine-performance instrument, not a verification capability.

A catalog of cases that are non-trivial for one engine and sit next to one of its budgets:
`exact-raster` (iteration-bound), `exact-twocount` (deep, growing set), `exact-forward` (the
twocount32 forward-reach pathology, abstains by design), `exact-relational` (representation-bound,
cell-major), `exact-mult` (multiplier bit-blast), `cube-rtl-i2c` (real RTL, |P| control predicates,
the hyper-must/post-image grind), plus references. `calibrate` sweeps each family's knob until the
run's CPU time lands in a window (60..360 s by default); `profile` runs the calibrated command under
samply (host, native speed, prints a self/inclusive callee map) or callgrind (the `mununu-profile`
image; a `.callgrind.out` for kcachegrind/qcachegrind). Build `target/profiling/mununu` first
(`cargo build --profile profiling -p mununu-cli`: release optimisation plus line tables).

```bash
scripts/profile_cases.py list
scripts/profile_cases.py calibrate exact-raster --min 60 --max 360 --cap 600
scripts/profile_cases.py profile exact-raster --size 8000 --tool samply
scripts/profile_cases.py profile exact-raster --size 400 --tool callgrind --toggle-collect '*ExactModel*'
```

The exact families disable the fixpoint soft bounds (`MUNUNU_BDD_FIXPOINT_{NODES,ITERS}=0`) and widen the
arena so the fixpoint itself runs to convergence; otherwise every size stops at the 10 M-node latency bound
around 17 s and the ranking certificate decides instead. Calibrations are written to
`target/profiles/<case>.calibration.json` with the full sweep.

## heat_check.py — did the hot spots move?

> Source of truth: [`scripts/heat_check.py`](heat_check.py) — surface: CLI-only — an engine-performance instrument, not a verification capability.

Run at the START of every optimization experiment and AFTER every kept one. Records the callee map
(self and inclusive share per function, from samply) of a fixed sub-10-minute corpus — about 55 s
native on the validation host — and compares it with the previous record, flagging any function whose
share moved more than a threshold and reporting the watched functions (the ones the candidate under
study targets) by name. A candidate aimed at a hot spot that has moved is re-read before it is built.

```bash
cargo build --profile heat -p mununu-cli                   # release opt, no LTO: ~3 min
scripts/heat_check.py run --label base --watch substitute   # the record before an experiment
# ... implement, rebuild the heat binary ...
scripts/heat_check.py run --label m1 --compare base --watch substitute
scripts/heat_check.py compare base m1 --threshold 10
```

Records land in `target/heat/<label>.json` with the profiles under `target/heat/<label>/`. The
verdict is `HEAT UNCHANGED` or `HEAT MOVED`; the work counter (`work N BDD ops` on the engine's
`MUNUNU_BDD_REPORT_WORK` line, which the script sets — no collection runs for it, so the map shows
engine time only) and the iteration count ride along per case.

When the map says the heat is inside OxiDD (`apply_rec`, `substitute`, `quant`, the apply cache,
`gc`) rather than in mununu, the callee map has nothing more to say — the next instrument is
OxiDD's own counters, below.

## OxiDD's per-operation counters — when the heat is inside the library

> Source of truth: [`crates/mununu-core/Cargo.toml`](../crates/mununu-core/Cargo.toml) (`oxidd-statistics = ["oxidd/statistics"]`) + `MUNUNU_BDD_STATS` in [`symbolic_bitblast.rs`](../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) — surface: CLI-only — a diagnostic build for engine work, never a shipped one.

The exact engine's time is in OxiDD's apply recursion, and a callee map cannot tell *why* a
recursion is expensive: whether the apply cache stopped hitting, whether the recursion is reaching
the terminals, or whether the diagram is simply large. OxiDD's `statistics` feature counts, per
operator (`And`, `Or`, `Xor`, `Ite`, `Subst`, `Exists`, …): the calls, the cache queries (calls
minus terminal cases), the cache hits, and the nodes created after reduction. mununu exposes it as
the `oxidd-statistics` cargo feature; `MUNUNU_BDD_STATS=1` prints the table to stderr once per
evaluation. It is a separate build on purpose: the counters are relaxed atomics on the hot path and
perturb the timings they exist to explain, so a statistics binary is never the one you time.

```bash
cargo build --profile profiling -p mununu-cli --features mununu-core/oxidd-statistics
cp target/profiling/mununu target/profiles/mununu-stats
MUNUNU_BDD_STATS=1 MUNUNU_BDD_REPORT_WORK=1 target/profiles/mununu-stats --quiet \
    btor2 verify-recoverability target/profiles/cases/exact-relational-11.btor2 --target 'done == 1' \
    2>&1 >/dev/null | grep -E '^  [A-Za-z]+: calls|work '
```

Each line reads `Op: calls: C, cache queries: Q (T % terminal cases), cache hits: H (R %),
reduced: N`. The counters are read-and-reset — the table shows the work since the previous print,
and `work N BDD ops` on the same stderr is the engine's own counter of the operations it issued,
so `calls / work` is the recursion's amplification per issued operation. What the counters have
settled so far, and the reading to take from each:

- **Calls are the cost.** The variable-order study measured 9.7× more `And` calls where the
  interleaved order lost (`sdram_burst`) and 77× fewer where it won (the barrel shifter), while the
  hit rate moved *opposite* to performance in both — better-but-slower, worse-but-faster. The
  apply-cache hypothesis was refuted by the counters, not confirmed; see
  [`docs/design/bdd-variable-ordering.md`](../docs/design/bdd-variable-ordering.md). So compare
  `calls` between two configurations before the hit rate, and treat a hit-rate change as a
  consequence of the recursion's shape, not its cause.
- **A bigger cache that does not move the hit rate is not a lever.** The apply-cache sizing
  experiment (M3, arena/4 → arena/2) left every hit rate unchanged and cost 340 MB; it was measured
  and not built. The counters are the cheap way to retire that class of candidate again.
- **Reproducible where the clock is not.** The call counts reproduced byte-identically across a
  3× load range on the consumer blocks, which is why they — not wall time — are the primary metric
  in the variable-order briefing.

The counters are per process, not per manager: the squarer's arena (`MUNUNU_BDD_SQUARING`) and the
engine's share one table, so read a squaring run with the rescue off first.
