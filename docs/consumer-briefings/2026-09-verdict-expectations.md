# Consumer briefing — assert the verdicts you claim (`--expect-*`), and a new exit code

> **Audience:** anyone gating CI on `mununu sv verify-auto`. **monono, ROSF.**
> Also: a **binding-rule change** to Surface Parity that affects every contributor — see the last
> section.

## TL;DR

Three assertions, on CLI and HTTP API:

```bash
# every property holds, nothing unsupported/unknown/skipped, and EXACTLY 8 of them
mununu sv verify-auto dut.sv --source dut_sva.sv --top dut --expect-all-hold --expect-count 8

# a contrast twin: these must be VIOLATED, every other property must still HOLD
mununu sv verify-auto faulty.sv --source dut_sva.sv --top dut --expect-violated dut_sva_sva_1

# exact per-property claims; an UNNAMED violated is still a failure
mununu sv verify-auto dut.sv --source dut_sva.sv --top dut --expect 'sva_0=holds,sva_1=unknown'
```

**New exit code `4` = "a verdict was not what you claimed."** Declaring an expectation supersedes
`--fail-on`.

Nothing changes for anyone who does not pass an `--expect-*` flag.

## Why upstream

Three consumers already exist and would each write these in shell — and would each independently
rediscover the same traps, including the lower-case-`skipped` regex bug that made a guard stop
firing exactly when it was needed. The vocabulary is about **verdicts**, which is mununu's domain;
nothing in it mentions any consumer's problem space.

## The verbs, and the failure each exists to catch

| flag | catches |
|---|---|
| `--expect-all-hold` | Stricter than `--fail-on unknown`: also rejects `skipped` — which the CI gate treats as a **pass** — and an assertion that did not translate. Both are properties that are **not being checked**. |
| `--expect-count N` | A binding that stopped binding produces **fewer** properties, and a smaller all-green set reads as a clean pass. Nothing else catches this, because a property that stopped being produced is invisible to every per-property check. |
| `--expect-violated a,b` | The named must be VIOLATED **and every other property must still HOLD**. A twin that breaks *everything* teaches nothing about which property covers which fault, so the second half is not optional. |
| `--expect 'a=VERDICT,…'` | Each named property returns exactly that verdict. Unnamed are ignored — **except** that an unnamed VIOLATED is still a failure, so the verb cannot become a way to ignore what you did not mention. |

Verdict spellings are **case-insensitive** (`holds` / `HOLDS` / `Holds`), and `⊥` is accepted for
`unknown`. A malformed `NAME=VERDICT` is a **usage error (exit 1)**, never a silently dropped
claim — a claim that does not run gates on nothing, which is worse than no claim at all.

## Exit codes

| exit | meaning |
|---|---|
| `0` | expectations met |
| `1` | the run failed (tool/usage error, including a malformed `--expect` pair) — still implies empty stdout |
| `4` | **an expectation was not met** |

`2` (violated) and `3` (unknown under `--fail-on unknown`) remain the ordinary gate for runs that
declare no expectations.

**Expectations supersede `--fail-on`, and they have to.** Under `--expect-violated` the ordinary
gate would exit `2` on the very violation you asked for, so a satisfied claim could never exit 0.
Note also a pre-existing collision worth knowing: clap's own argument-parse failures exit `2`.

## Pinning a ⊥ is the sound way to record one

`--expect 'sva_1=unknown'` records an abstention as a **claim**, and the claim fails when the
property becomes decidable. **That is the point**, not an annoyance: monono's `video_timing` twin
pinned `sva_1=UNKNOWN`, mununu#503 made it decidable, and the gate failed on the next run — an
upstream improvement surfaced as a signal instead of vanishing into silence.

### There is deliberately no "tolerate undecided" flag

One was written downstream (`expect_all_hold_except`) and **deleted**. It was introduced for
`sdram_ctrl`'s three abstaining assertions; those turned out to be **false rather than hard**. The
engine had abstained, the excuse made the abstention comfortable, and three wrong assertions sat
behind it until they were restated decidably and all nine held at real timings.

If a genuinely-hard property ever needs an exemption, it should be argued for on its own terms —
not reintroduced as a convenience. A ⊥ should keep feeling like "not checked".

## Per-consumer

### monono

- **What to update:** `verify/lib.sh`'s three helpers become flags. `expect_all_hold` →
  `--expect-all-hold [--expect-count N]`; `expect_violated` → `--expect-violated`; `expect_named`
  → `--expect`. Treat exit `4` as the assertion failure and exit `1` as the run failure — a
  distinction the shell version could not make, since it could only see "non-zero".
- **What to expect:** identical verdicts. The `--json` output gains
  `expectations: { satisfied, failures[] }`, so a failing check reports *which* claim broke
  without any transcript parsing.

**The mapping, against a real block** (`rtl/vpu/tmds_serialiser/verify.sh`, which uses two of the
three verbs):

```bash
# before
expect_all_hold "tiers 1+2+3 all hold" \
    "$HERE/tmds_serialiser.sv" --source "$HERE/tmds_serialiser_sva.sv" --source "$STUBS" \
    --top tmds_serialiser --config-value rst_n=1

# after — the invocation is unchanged; only the assertion moves
mununu sv verify-auto "$HERE/tmds_serialiser.sv" \
    --source "$HERE/tmds_serialiser_sva.sv" --source "$STUBS" \
    --top tmds_serialiser --config-value rst_n=1 \
    --expect-all-hold
```

```bash
# before
expect_violated "faulty twin: four shift cycles instead of five" \
    "tmds_serialiser_sva_sva_1" -- "$HERE/faulty/tmds_serialiser_short_phase.sv" ...

# after
mununu sv verify-auto "$HERE/faulty/tmds_serialiser_short_phase.sv" ... \
    --expect-violated tmds_serialiser_sva_sva_1
```

Note what does **not** move: sources, stubs, `--top`, `--config-value`, the label, and the
PASS/FAIL formatting. Your runner already expresses the invocation well — only the assertion
belonged upstream, which is why this shipped as flags rather than as a contract file that would
have had to duplicate the rest.

Two behaviours you can now delete rather than reimplement: the `--config-value` transcript grep
(unusable pins are a hard error upstream), and the case-insensitivity workaround for
lower-case `skipped` (verdict spellings are matched case-insensitively here).

### ROSF

- **What to update:** nothing required. To use it over HTTP, add an `expectations` object to the
  request (`all_hold`, `count`, `violated`, `named`); the response carries
  `expectations: { satisfied, failures[] }`. A bad verdict spelling is a `400`.

### mununu-ui

- **What to update:** nothing. No UI is expected for this — see below.

## Binding-rule change: Surface Parity

**Surface Parity now requires CLI + HTTP API for every capability, and UI parity only for
CTXDSL-related capabilities** (those whose *subject* is a CTXDSL model — authoring, evaluating,
composing, visualizing, synthesizing from, or importing into one).

For everything else — RTL / SVA verification verbs, contract / black-box tooling, and CI-gate
ergonomics like this feature — **a missing UI affordance is no longer drift**. A UI control for
"assert this run produced exactly these verdicts" would be an affordance with no user: CI lanes
and orchestrators consume it, not someone at a canvas.

This does **not** remove existing UI. It removes the obligation to *grow* it for work that is not
about CTXDSL. `CLAUDE.md` §Surface Parity and `.claude/skills/parity-check/SKILL.md` are updated
together, so the tooling and the rule agree; `/parity-check` now marks such rows `n/a` instead of
reporting them as gaps.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | Rust only; no tool pins touched | No |
| `mununu-sva` | Inherits `mununu-dev`; tool pins unchanged | No |
| `mununu-sva-pono` | Inherits `mununu-sva`; tool pins unchanged | No |
| `hw-verif` | Not involved | No |

## Test the transition

```bash
# should exit 0
mununu sv verify-auto dut.sv --source dut_sva.sv --top dut --expect-all-hold; echo $?
# should exit 4 and name the property
mununu sv verify-auto dut.sv --source dut_sva.sv --top dut --expect 'sva_0=violated'; echo $?
```

The semantics is covered by 18 unit tests over hand-built reports — no toolchain needed — each
verb with a **negative control**, because an evaluator that always returned "satisfied" would pass
every positive test. Both were mutation-tested: disabling the unnamed-VIOLATED rule fails exactly
one test, and an always-satisfied evaluator fails 11 of 18.

## Provenance

- Issue: [mununu#537](https://github.com/Mumunu-team/mununu/issues/537). Depends on
  [#536](https://github.com/Mumunu-team/mununu/issues/536), which made the report machine-readable.
- Implementation: `crates/mununu-core/src/adapter/slang/expectations.rs` (the pure evaluator),
  `ExpectArgs` in `crates/mununu-cli/src/main.rs`, `ExpectationsRequest` /
  `ExpectationResultView` in `crates/mununu-core/src/api/models.rs`.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- **No contract file.** The assertions are flags. A file beside the sources would either duplicate
  the invocation a consumer's runner already expresses, or become a new format to version and
  drift-check. If per-design storage turns out to be wanted, it can desugar to the same evaluator.
- **No resume / no partial expectations.** The claims are evaluated against a completed report.
- **`--expect-count` counts translated properties**, not assertions in the source; an assertion
  that did not translate appears in `unsupported[]` and fails `--expect-all-hold` separately.
