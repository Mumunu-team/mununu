---
name: failure-path-audit
description: >
  Audits mununu's engine and adapter code for the three failure-path classes the
  issue history recurs on: a crash where an abstention was due (unwrap / expect /
  panic / unchecked index in non-test engine code), silence (an error, refusal,
  budget stop or fallback discarded without reaching the report), and a remedy the
  user cannot take (a flag no verb has, internal vocabulary, an "unsupported" with
  no owner). Reports file:line, the class, and the abstention, report field or
  message the site should reach. Read-only. Use when reviewing changes under
  crates/mununu-core/src/adapter or to any user-facing message, or as a sub-step
  of review-orchestrator.
---

Perform a failure-path audit of $ARGUMENTS (or changed files via `git diff --name-only HEAD~1 HEAD | grep '\.rs$'` if no args). The rule being enforced is `CLAUDE.md` → **Failure Paths** (full text: `docs/policies/failure-paths.md`).

mununu's verdicts must be sound AND its silence honest: an engine that cannot finish abstains with a reason, and every path on which the tool does less than it was asked says so in the report. The issue history shows both classes recur as chains when fixed one site at a time (crash: #462 → #490 → #504 → #542 → #600 → #638 → #730; silence: #459, #466, #467, #553, #596, #601, #723, #734, #764). This audit lists the sites so a fix sweeps the class.

## Scope

Engine and adapter code under `crates/mununu-core/src/adapter/`, `crates/mununu-core/src/planner/`, `crates/mununu-core/src/mu_calculus/`, and the N-source orchestrator under `crates/mununu-core/src/verify/` (it merges per-source verdicts, so a dropped error there is a dropped verdict). Test modules (`#[cfg(test)]`, `tests/`) are out of scope: a test may unwrap.

## Class A — crash where an abstention was due

Search (non-test code only):

```
\.unwrap\(\)            # excluding `.unwrap_or`, `.unwrap_or_else`, `.unwrap_or_default`
\.expect\(
panic!\(|unreachable!\(|todo!\(|unimplemented!\(
\[[a-z_]+\]             # an index by a computed value with no bounds check nearby (bit positions, widths, nids)
as u32\)|as usize\)     # a width or count narrowed from a wider type on a path a design can widen
```

For each site decide, from the surrounding code, which of three it is:

1. **An invariant the code itself established** (the key was inserted ten lines up; the sort was checked) — acceptable; recommend `expect("<the invariant>")` naming it, so a future violation reads as a message, not an index.
2. **A resource or input the design controls** — OxiDD `OutOfMemory` / node budget, a width over the cap, a sort the encoder does not take, a constant that does not fit, recursion on the design's depth. **HIGH.** The fix is a typed error that reaches the existing abstention path: `AdapterErrorKind::ResourceBudgetExceeded`, `BottomReason::{BitCap, ArenaExhausted, …}`, or a named refusal the portfolio records.
3. **Unknown** — report it; the reviewer decides.

Modules with `#![deny(clippy::unwrap_used, clippy::expect_used)]` are already clean for the first pattern; report any `#[allow(clippy::unwrap_used)]` inside them with its justification comment (missing comment = finding).

## Class B — silence: a failure path that reaches no report

Search:

```
Err\(_\) => None|Err\(_\) => \{\}|Err\(_\) => (return )?(Vec::new\(\)|false|0|None)
\.ok\(\)(?!\s*\?)       # a Result discarded into an Option on a fallible call
\.ok\(\)\.flatten\(\)
let _ = [a-z_:]+\(      # a fallible call whose result is dropped
unwrap_or_default\(\)|unwrap_or\(Vec::new\(\)\)
if let Ok\(            # with no `else` branch that reports
```

For each site ask: **can a consumer tell this path was taken?** Acceptable when the discarded error is re-surfaced somewhere the consumer reads (a `BottomReason`, a `VerificationNote`, a `ModelDiagnostics` field, the member's `refusals` list — cite the line). **HIGH** when the path changes what the report claims (a verdict, a count, a list of sites) and leaves no trace. Known precedents of the HIGH form: `run_native`/`run_spacer` mapping `Err → None` (#734); `--config-value` matching nothing (#459); a trace cell wider than 64 bits dropped (#723).

Also flag a new budget, cap, ceiling, skip or fallback (`if … > MAX_`, `return None` after a size test, a second frontend/engine tried after the first failed) that adds no `BottomReason` / `SkipReason` arm, note kind or `diagnostics.*` field in the same change.

## Class C — a remedy the user cannot take

Rule 5 of the policy (2026-11-19, mununu#1106). In the same scope plus `crates/mununu-core/src/verify/`, `composition/`, `api/` and `crates/mununu-cli/src/`, read every string that reaches a user and tells them what to do: the message of an `AdapterError` / `AdapterWarning`, a `BottomReason` or `VerificationNote` text, an API error body, a CLI diagnostic. Search:

```
--[a-z][a-z0-9-]+        # a flag inside a message string
MUNUNU_[A-Z0-9_]+        # an environment variable inside a message string
\{[a-z_]*:\?\}            # Debug output in a format string that builds user text
sidecar|reshape|rewrite|lift the|use `|try |supply |install |increase |raise
unknown |unrecognised |unsupported |not supported
Phase [0-9]|Tier-?[0-9]|step [0-9]+\.[0-9]|[A-Z][a-z]+::[A-Z][A-Za-z]+   # roadmap names and Rust paths
```

For each site decide:

1. **Unfollowable — HIGH.** The flag is on no clap verb that can reach this code (grep `crates/mununu-cli/src/main.rs`), or only on a different verb; the variable is absent from `docs/env-vars.md`; the key or construct is one the parser rejects; the mechanism named was retired. Cite the line that shows it.
2. **No remedy and no owner — MEDIUM.** The message stops at "unsupported" / "cannot" without saying whether this is a limit of the technique, a mununu limitation, or a mununu defect to report.
3. **Internal vocabulary — MEDIUM.** A Rust identifier, `{:?}` of an internal value, a roadmap step, a phase or a tier name in user text; positions like "the left member" where the input's own name is in scope.
4. **No candidates — LOW.** An "unknown X" that lists nothing, with the candidate set in scope.

A message in code shared by several verbs is checked against each verb that reaches it. Do not rewrite the message in the report beyond one proposed line; the fix belongs to the change.

## Output format

Group by class, then by file. One line per site:

```
A2 HIGH  crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs:1094  `.unwrap()` on OxiDD `and` — OutOfMemory reaches the user as exit 101; should become `ResourceBudgetExceeded` → ⊥ `arena-exhausted` (#600 class)
B  HIGH  crates/mununu-core/src/adapter/reach_portfolio.rs:412           `run_interp(..).ok()` — the member's refusal reason is lost; record it in `ReachOutcome.refusals` (#734 class)
```

A class C line reads `C1 HIGH  <file:line>  names `--flag` — on no verb that reaches this code (<evidence line>); say <one proposed line>`.

End with the counts per class and severity, and — when the scope is a changed-files diff — the sites the change ADDED (a new site is a review finding even at LOW).

## What this audit is not

It does not judge soundness directions (`/soundness-check` does), nor test coverage (`/test-coverage`). A site that is acceptable here may still need a `// SOUNDNESS:` comment there.
