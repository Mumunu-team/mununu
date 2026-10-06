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
parses the exact engine's `peak … in K iteration(s)` line so the host-independent iteration
count is compared before the wall clock.

```bash
scripts/ab_time.py preflight                       # load, heavy apps, power, thermal, running builds
MUNUNU_BDD_REPORT_PEAK=1 scripts/ab_time.py run before -n 10 --caffeinate -- \
    target/release/mununu --quiet btor2 verify-recoverability d.btor2 --target "s == 0"
# change, rebuild, then:
MUNUNU_BDD_REPORT_PEAK=1 scripts/ab_time.py run after  -n 10 --caffeinate -- <same command>
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
`MUNUNU_BDD_REPORT_PEAK` line) and the iteration count ride along per case.
