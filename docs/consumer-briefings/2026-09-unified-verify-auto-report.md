# Consumer briefing — `sv verify-auto --json` is now the same document as the API response

> **Audience:** anyone parsing `mununu sv verify-auto` output. **monono, ROSF, mununu-ui.**
>
> **Read this if you parse the transcript.** You do not need to, and should not — see
> *"If you are parsing the transcript"* below.

## TL;DR

`mununu sv verify-auto --json` and `POST /api/v1/sv/verify-auto` now emit the **same document**,
described by [`sv-verify-auto-response.schema.json`](../api-schemas/sv-verify-auto-response.schema.json)
and pinned by a drift test that now covers both surfaces.

Three things changed for consumers:

1. **New structured fields** — the ⊥/violated cell counts, the skip reason, the front end used, the
   applied `--config-value`s, the cutpoints, and the counterexample's input trace.
2. **One breaking change on the CLI** — `properties[].detail` is now a string, not an object. The
   number it used to hold is a first-class field.
3. **A wrong count is fixed** — the `coverage-summary` note could disagree with the verdicts on a
   portfolio run, which is the default. If you counted that note, your numbers were wrong.

## `--json` already existed

Worth stating plainly, because [mununu#536](https://github.com/Mumunu-team/mununu/issues/536) was
filed as *"offers no `--report json`"*: `sv verify-auto --json` has shipped since verify-auto
itself (#169, 2026-06-27) and is advertised in `--help`. If you built transcript parsing, you can
delete it today.

## What was actually broken

There were **two hand-written serializers for one documented shape** — the CLI built a
`serde_json::json!` literal, the API built the response struct field by field — and only the API
shape was described by the published schema. So a consumer reading the schema and then running the
CLI got a different document. They had already drifted:

| | CLI `--json` (before) | API (before) |
|---|---|---|
| `properties[].detail` | object: `{"unknown_cells": 32768}` | string: `"32768 cell(s)"` |
| `counterexample.unreachable_target` | absent | present |
| `counterexample.inputs` | absent | absent |
| `diagnostics.frontend` | absent | absent |

Two serializers for one shape cannot be kept in step by discipline. There is now exactly one
(`impl From<&AutoVerifyReport> for SvVerifyAutoResponse`), so the schema drift test guards both.

## The breaking change, stated plainly

**`properties[].detail` on the CLI changes from an object to a string.**

```jsonc
// before (CLI only)
{ "outcome": "unknown", "detail": { "unknown_cells": 32768 } }
// after (CLI and API)
{ "outcome": "unknown", "detail": "32768 cell(s)", "unknown_cells": 32768 }
```

Net strictly better — the number is now a first-class field on *both* surfaces instead of an object
on one and a sentence on the other — but it is a shape change, not an addition. It is justified
because the alternative was keeping two shapes forever, and because the known consumer of this
output parses the transcript rather than the JSON. If you did parse CLI `--json`, read
`unknown_cells` / `false_cells` / `skip_reason` instead of `detail`.

Everything else is **additive**. The API response gains fields and loses none.

## New fields

| field | what it answers |
|---|---|
| `properties[].unknown_cells` | the ⊥ figure, as a number — track whether refinement is helping |
| `properties[].false_cells` | the violated cell count, as a number |
| `properties[].skip_reason` | why a `skipped` property was not evaluated |
| `diagnostics.frontend` | **which lift produced this verdict** (#466). The two front ends differ in soundness on partial writes (#464/#465), so a silent choice mattered; it was previously only note prose |
| `diagnostics.frontend_fallback_reason` | if `--frontend auto` fell back, the earlier attempt's error |
| `diagnostics.config_values` | the `--config-value` pins actually applied — a verdict under a pin is a claim about **that** configuration only |
| `diagnostics.cutpoints` | the control slice these verdicts were reached under |
| `counterexample.inputs` | the per-cycle input assignment, so a trace can be **replayed** against the RTL rather than only read |

## The count that was wrong

`merge_portfolio_reports` starts from `base.clone()` — the highest-precision engine — and that
clone brought the base's **notes** with it. The merge then rewrote `properties[].outcome` with the
first definite verdict across all engines, but nothing rewrote the notes. So the `coverage-summary`
reported the *base engine's* tally while `properties[]` reported the merged one.

`portfolio-sequential` is the **default** engine, so this was the common case. It is exactly what
mununu#498's minor and #536 observed: a summary claiming `0 unknown` beside properties printing
`UNKNOWN`.

**If your gate counted the `coverage-summary` note, it under-reported your coverage.** It now
counts the merged verdicts. Expect the numbers in that note to change — to the correct ones.

## The rule that prevents the next one

**`properties[]` is the verdict record. `notes[]` is commentary and must never be counted.**

A note is prose for a human skimming a terminal. An assertion absent from `properties[]` is not
being checked — look for it in `unsupported[]`, which carries mununu's own reason
(`"unsupported binary op: BinaryAnd"`). This is now stated in
[`docs/api-schemas/verdict.md`](../api-schemas/verdict.md) with a field-by-field table of what to
read instead of what to parse.

## If you are parsing the transcript

Stop. The transcript's layout is not a contract and has already broken a gate: an earlier regex
captured `[A-Z]*` only, so a `skipped` property — which mununu prints in lower case — came back
with an **empty** verdict, which the harness read as "absent" and its uncovered-counter did not
count at all. A guard that stops firing exactly when it is needed.

```bash
# every property and its verdict
mununu --quiet sv verify-auto design.sv --json | jq -r '.properties[] | "\(.name) \(.outcome)"'
# what did NOT translate, and why
mununu --quiet sv verify-auto design.sv --json | jq -r '.unsupported[] | "\(.name): \(.reason)"'
```

## Per-consumer

### monono

- **What to update:** replace `_verdicts()` / `_uncovered()` / `_unsupported_reasons()` in
  `verify/lib.sh` with `jq` over `--json`. The lower-case-`skipped` bug cannot recur.
- **What to expect:** no verdict changes. The `coverage-summary` note's numbers become correct on
  portfolio runs.

### ROSF

- **What to update:** nothing required; the API response is additive. Adopt `unknown_cells` /
  `frontend` when convenient.

### mununu-ui

- **What to update:** nothing required — additive. `PropertyVerdictView` and
  `ModelDiagnosticsView` gained optional fields; `src/api/endpoints.ts` can adopt them when the UI
  wants to show the front end used or the ⊥ figure.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | Rust only; no tool pins touched | No |
| `mununu-sva` | Inherits `mununu-dev`; tool pins unchanged | No |
| `mununu-sva-pono` | Inherits `mununu-sva`; tool pins unchanged | No |
| `hw-verif` | Not involved | No |

## Test the transition

```bash
mununu --quiet sv verify-auto design.sv --json | jq -e '.properties | length > 0'
mununu --quiet sv verify-auto design.sv --json | jq '.diagnostics.frontend'
```

Covered by `a_serialized_report_conforms_to_the_published_response_schema`, which serializes a real
report through the shared conversion and checks it against the published schema **in both
directions** — every `required` key present, and every emitted key declared. A one-directional
check would have missed the failure that actually happened: a serializer emitting a field the
schema never mentioned. The count fix is pinned by
`portfolio_merge_recounts_the_coverage_summary`, which fails without it.

## Provenance

- Issue: [mununu#536](https://github.com/Mumunu-team/mununu/issues/536); the count fix also closes
  [mununu#498](https://github.com/Mumunu-team/mununu/issues/498)'s minor.
- Implementation: `impl From<&AutoVerifyReport> for SvVerifyAutoResponse`
  (`crates/mununu-core/src/api/models.rs`), `refresh_coverage_summary`
  (`crates/mununu-core/src/adapter/slang/verify_auto.rs`).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- **`unsupported[].kind` is still always `null`.** The core report flattens to `(name, reason)`
  before the wire layer, so the SVA kind is gone by then. Fixing it means widening the core type.
- **`reason` has no machine-stable taxonomy.** It is human-authored prose, so classifying
  "unsupported binary op" vs "unbounded repetition" still needs string matching. A reason *code*
  would be a separate change.
- **Verdict expectations** — asserting that a run produced the verdicts you claimed — are
  [mununu#537](https://github.com/Mumunu-team/mununu/issues/537), landing next.
