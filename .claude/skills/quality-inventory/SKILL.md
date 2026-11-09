---
name: quality-inventory
description: >
  Collects code quality metrics at project, module, and file levels, or
  installs measurement tools to upgrade the metrics tier when invoked with
  `--setup`. Detects available tool tier and adapts measurement accordingly.
  Use when asked to measure, inventory, baseline, or set up code quality.
---

## Mode dispatch

Inspect `$ARGUMENTS`:

- If it contains `--setup` (with or without a tier specifier), jump to **Setup mode** below.
- Otherwise, run the default **Inventory mode** on the scope (or `crates/` if no scope is given). Strip any `--` flags before treating the remainder as a scope path.

## Inventory mode

Collect quality metrics for the given scope. Output a structured inventory.

## Tool Tier Detection

Before collecting, detect available tools and report the tier:

```bash
# Check each tool — report tier in output
command -v tokei >/dev/null 2>&1 && echo "tokei: available" || echo "tokei: missing"
command -v rust-code-analysis-cli >/dev/null 2>&1 && echo "rust-code-analysis: available" || echo "rust-code-analysis: missing"
command -v cargo-modules >/dev/null 2>&1 && echo "cargo-modules: available" || echo "cargo-modules: missing"
command -v cargo-mutants >/dev/null 2>&1 && echo "cargo-mutants: available" || echo "cargo-mutants: missing"
command -v cargo-geiger >/dev/null 2>&1 && echo "cargo-geiger: available" || echo "cargo-geiger: missing"
command -v cargo-udeps >/dev/null 2>&1 && echo "cargo-udeps: available" || echo "cargo-udeps: missing"
command -v cargo-nextest >/dev/null 2>&1 && echo "cargo-nextest: available" || echo "cargo-nextest: missing"
```

- **Tier 0** (always available): grep, wc, cargo clippy, cargo test
- **Tier 1** (+tokei, +rust-code-analysis-cli): proper SLOC, cyclomatic/cognitive complexity
- **Tier 2** (+cargo-modules, +cargo-geiger, +cargo-udeps): coupling, unsafe surface, dead deps
- **Tier 3** (+cargo-mutants, +cargo-nextest): mutation testing, parallel test execution

## Tier 0 Metrics (always collected)

### Project Level

```bash
# Total SLOC (Rust files only, exclude tests and benches)
find crates/ -name '*.rs' ! -path '*/tests/*' ! -path '*/benches/*' | xargs wc -l | tail -1

# Crate count
ls -d crates/*/Cargo.toml 2>/dev/null | wc -l

# Total test count
cargo test --workspace -- --list 2>&1 | grep ': test$' | wc -l

# Clippy warnings (workspace)
cargo clippy --workspace --all-targets --message-format=json 2>&1 | grep '"level":"warning"' | wc -l

# Unsafe block count (non-test code)
grep -r 'unsafe ' crates/ --include='*.rs' -l 2>/dev/null | grep -v '#\[cfg(test)\]' | wc -l
```

### Module Level (per top-level directory under src/)

For each module directory, collect:

```bash
# SLOC
find $MODULE_PATH -name '*.rs' | xargs wc -l | tail -1

# Function count
grep -r '^[[:space:]]*\(pub \)\?fn ' $MODULE_PATH --include='*.rs' | wc -l

# Public items
grep -r '^[[:space:]]*pub ' $MODULE_PATH --include='*.rs' | wc -l

# Test module presence (files with #[cfg(test)])
grep -rl '#\[cfg(test)\]' $MODULE_PATH --include='*.rs' | wc -l

# Files WITHOUT test modules
total=$(find $MODULE_PATH -name '*.rs' ! -name 'mod.rs' | wc -l)
tested=$(grep -rl '#\[cfg(test)\]' $MODULE_PATH --include='*.rs' | wc -l)
echo "untested files: $((total - tested))"

# Unwrap count (non-test code — approximate by excluding lines after #[cfg(test)])
grep -rn '\.unwrap()' $MODULE_PATH --include='*.rs' | grep -v '#\[test\]' | grep -v '#\[cfg(test)\]' | wc -l

# Clone count (non-test)
grep -rn '\.clone()' $MODULE_PATH --include='*.rs' | grep -v '#\[test\]' | grep -v '#\[cfg(test)\]' | wc -l

# todo!/unimplemented! count
grep -rn 'todo!\|unimplemented!' $MODULE_PATH --include='*.rs' | wc -l
```

### File Level (per .rs file)

For each file, collect:

```bash
# SLOC
wc -l $FILE

# Top-level item count (fn, struct, enum, trait, impl, type, const, static)
grep -c '^[[:space:]]*\(pub \)\?\(fn\|struct\|enum\|trait\|impl\|type\|const\|static\) ' $FILE

# Has test module?
grep -c '#\[cfg(test)\]' $FILE

# Max nesting depth (approximate: count leading whitespace)
awk '{ match($0, /^[[:space:]]*/); depth=RLENGTH/4; if(depth>max) max=depth } END { print max }' $FILE

# Import count
grep -c '^use ' $FILE
```

### Function Level (files flagged as large)

For files over the SLOC threshold, extract per-function metrics:

```bash
# List functions with line counts (approximate: lines between fn signatures)
grep -n '^[[:space:]]*\(pub \)\?\(async \)\?fn ' $FILE
```

Count lines between consecutive `fn` signatures to estimate function length. Flag functions exceeding thresholds from `.quality/thresholds.toml`.

### Dependency Direction Check

```bash
# Core modules must not import adapter code
for mod in clts ltl mu_calculus composition context; do
  grep -rn 'use crate::adapter\|use super::.*adapter' crates/mununu-core/src/$mod/ --include='*.rs' 2>/dev/null
done
```

Flag any matches as DIP violations.

## Tier 1+ Metrics (when tools available)

If `tokei` is available: use it instead of `wc -l` for accurate SLOC (excludes comments/blanks).

If `rust-code-analysis-cli` is available:
```bash
rust-code-analysis-cli -m -p $FILE -O json
```
Extract cyclomatic complexity, cognitive complexity, Halstead metrics per function.

If `cargo-modules` is available:
```bash
cargo modules generate tree --lib -p mununu-core
```
Extract module dependency graph for coupling analysis.

If `cargo-geiger` is available:
```bash
cargo geiger --output-format=json
```
Extract precise unsafe surface area.

If `cargo-udeps` is available:
```bash
cargo +nightly udeps --workspace --output json
```
List unused dependencies.

## Output Format

Report as structured text with clear sections:

```
## Quality Inventory — {scope}
Tier: {0|1|2|3}
Date: {YYYY-MM-DD}

### Project Summary
| Metric | Value |
|--------|-------|
| SLOC | N |
| Crates | N |
| Tests | N |
| Clippy warnings | N |
| Unsafe files | N |

### Module Breakdown
| Module | SLOC | Fns | Pub | Tested | Untested | unwrap | clone | todo |
|--------|------|-----|-----|--------|----------|--------|-------|------|
| ... | ... | ... | ... | ... | ... | ... | ... | ... |

### Flagged Files (over thresholds)
- file.rs: SLOC=N (threshold: M), pub_items=N (threshold: M)

### Flagged Functions (over thresholds)
- file.rs:fn_name: ~N lines (threshold: M)

### DIP Violations
- file.rs:L — core module imports adapter

### Missing Tool Tiers
To unlock more metrics, run `/quality-inventory --setup` to install Tier {next} tools.
```

When called from the quality-session agent, also write the raw data to the path specified in $ARGUMENTS (e.g., `.quality/sessions/<id>/before.json`).

## Setup mode

Install measurement tools to upgrade the inventory tier. Detect the current tier first, then install the next tier's tools.

### Tier detection

```bash
echo "=== Current Tool Availability ==="
command -v tokei >/dev/null 2>&1 && echo "✓ tokei" || echo "✗ tokei"
command -v rust-code-analysis-cli >/dev/null 2>&1 && echo "✓ rust-code-analysis" || echo "✗ rust-code-analysis"
command -v cargo-modules >/dev/null 2>&1 && echo "✓ cargo-modules" || echo "✗ cargo-modules"
command -v cargo-geiger >/dev/null 2>&1 && echo "✓ cargo-geiger" || echo "✗ cargo-geiger"
command -v cargo-udeps >/dev/null 2>&1 && echo "✓ cargo-udeps" || echo "✗ cargo-udeps"
command -v cargo-mutants >/dev/null 2>&1 && echo "✓ cargo-mutants" || echo "✗ cargo-mutants"
command -v cargo-nextest >/dev/null 2>&1 && echo "✓ cargo-nextest" || echo "✗ cargo-nextest"
```

### Tier 1 — SLOC & complexity

```bash
cargo install tokei
cargo install rust-code-analysis-cli
```

**Unlocks**: accurate SLOC (comments/blanks excluded), cyclomatic complexity, cognitive complexity, Halstead metrics per function.

### Tier 2 — Coupling & unsafe surface

```bash
cargo install cargo-modules
cargo install cargo-geiger
cargo install cargo-udeps
```

**Unlocks**: module dependency graph, afferent/efferent coupling, instability index, precise unsafe surface area, unused dependency detection.

`cargo-udeps` requires nightly: `rustup install nightly` if not already present.

### Tier 3 — Mutation testing

```bash
cargo install cargo-mutants
cargo install cargo-nextest
```

**Unlocks**: mutation testing scores, parallel test execution, per-test timing.

### After installation

Re-run `/quality-inventory` (without `--setup`) to verify the new tier is detected and additional metrics are collected. Report which tier was achieved and what new metrics are now available.
