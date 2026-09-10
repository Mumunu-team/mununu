# API schemas — verdict + verify-auto response

> **Stability:** evolving — see the [stability contract](#stability-contract) at the end of this document. Additions arrive additively (new optional fields); breaking changes ship with a version bump and a consumer briefing under [`../consumer-briefings/`](../consumer-briefings/).
>
> **Source of truth:** [`crates/mununu-core/src/verdict.rs`](../../crates/mununu-core/src/verdict.rs) (types) + [`crates/mununu-core/src/api/models.rs`](../../crates/mununu-core/src/api/models.rs) (JSON views). This document mirrors those types; a divergence between this doc and the code is a `doc`-severity finding.
>
> **Machine-readable schemas** are shipped alongside this file:
> [`sv-verify-auto-request.schema.json`](sv-verify-auto-request.schema.json) +
> [`sv-verify-auto-response.schema.json`](sv-verify-auto-response.schema.json)
> (JSON Schema Draft-07, derived from the Rust types via `schemars`). Consumers
> should code against these for type-generation and validation; this document
> covers the semantics the schema can't express. See [`README.md`](README.md)
> for the consumer flow and drift-detector guarantee.
>
> **Audience:** downstream consumers that parse mununu's `POST /api/v1/sv/verify-auto` JSON responses — ROSF (subprocess `--profile industrial`), monono (direct CLI/API), any custom orchestrator.

Mununu's verify surfaces (CLI, HTTP API, UI) all speak one verdict vocabulary. This document pins down the JSON wire format for the `sv verify-auto` response — the shape downstream tools should code against.

> **mununu#536 — this shape is also the CLI's.** `mununu sv verify-auto --json` emits the *same document* as the HTTP response; both go through one conversion, so the schema and the drift test cover both surfaces. Do not parse the human transcript: its layout is not a contract and has already broken a consumer's gate.

### Which field is authoritative

**`properties[]` is the verdict record. `notes[]` is commentary and must never be counted.**

This needs saying because it has bitten a consumer. A note is prose written for a human skimming a terminal — the `coverage-summary` note in particular is a convenience tally, and a harness that counted it under-reported its own coverage. `properties[]` is the answer: one entry per translated assertion, and an assertion **absent** from it is not being checked (look for it in `unsupported[]`, which carries mununu's own reason).

Read the figures from the structured fields, not from prose:

| you want | read | not |
|---|---|---|
| the verdict | `properties[].outcome` | the `[assert]` transcript line |
| the ⊥ cell count | `properties[].unknown_cells` | `detail` (a sentence containing the number) |
| the violated cell count | `properties[].false_cells` | `detail` |
| why a property was skipped | `properties[].skip_reason` | `detail` |
| why an assertion is missing | `unsupported[].reason` | the `[unsupported]` transcript line |
| a scope caveat to act on | `notes[].level == "scope-caveat"` | string-matching `summary` |
| what the run was scoped to | `diagnostics.config_values` / `.cutpoints` / `.frontend` | the `config-concretization` / `control-slice` / `lift-frontend` note prose |

`detail` remains for existing consumers, but it is prose and should be treated as such.

## Contents

1. [PropertyVerdict — the four canonical outcomes](#propertyverdict--the-four-canonical-outcomes)
2. [SvVerifyAutoResponse — top-level shape](#svverifyautoresponse--top-level-shape)
3. [PropertyVerdictView — per-property](#propertyverdictview--per-property)
4. [Counterexample — CexCellView / CounterexampleView](#counterexample--cexcellview--counterexampleview)
5. [Model diagnostics + notes](#model-diagnostics--notes)
6. [Unsupported assertions](#unsupported-assertions)
7. [DiscoveredAssumption — refinement-lane fields](#discoveredassumption--refinement-lane-fields)
8. [Stability contract](#stability-contract)
9. [Consumer guidance](#consumer-guidance)

## PropertyVerdict — the four canonical outcomes

Every verify surface reports one of four values as a lowercase string. Wire format:

| Value | Semantics |
|-------|-----------|
| `"holds"` | The property holds on every reachable state / path — a sound proof. Definite. |
| `"violated"` | The property is violated — a real counterexample exists. Definite. |
| `"unknown"` | No engine decided within budget: abstention, over-cap, timeout, or a soundness alarm (contradiction between engines). Definite in intent — never a hidden `Holds`/`Violated`. |
| `"skipped"` | The property was not evaluated: out of the supported fragment, refused by the engine's soundness guard (input-derived antecedent, partial-write havoc, etc.), or filtered pre-run. |

Rust source: [`PropertyVerdict`](../../crates/mununu-core/src/verdict.rs#L22) — `as_str()` at line 155 is the canonical mapping.

**Never treat `skipped` or `unknown` as `holds` in a CI gate.** The default `--fail-on violated` treats both as pass; `--fail-on none` treats both as fail. Choose the polarity your gate actually wants.

## SvVerifyAutoResponse — top-level shape

Rust source: [`SvVerifyAutoResponse`](../../crates/mununu-core/src/api/models.rs#L1691).

```jsonc
{
  "properties": [ /* PropertyVerdictView[] — see below */ ],
  "unsupported": [ /* UnsupportedAssertionView[] — SVA outside the fragment */ ],
  "diagnostics": {
    "state_register_count": 42,
    "blackboxed_modules":   ["some_missing_module"],
    "gated_resets":         ["rst_n=1"],
    "auto_provided_stubs":  ["prim_sparse_fsm_flop"]
  },
  "notes": [ /* VerificationNoteView[] — provenance/scope/soundness notes */ ]
}
```

## PropertyVerdictView — per-property

Rust source: [`PropertyVerdictView`](../../crates/mununu-core/src/api/models.rs#L1743).

```jsonc
{
  "name":               "my_assertion",
  "kind":               "assert",              // "assert" | "assume" | "cover"
  "formula":            "nu X. ((!req || [] grant) && [] X)",  // mu-calc lift of the SVA
  "outcome":            "holds",               // canonical PropertyVerdict as-string
  "detail":             "cube: 0 false / 12 unknown", // optional; skip reason, cell counts
  "seeded_predicates":  ["req == 1", "grant == 1"],   // atoms auto-seeded for this property
  "counterexample":     null                   // present only for exact-symbolic Violated on AF-shape
}
```

**Field-by-field:**

- `name` — SVA label from the source (`assert property (foo) …` → `"foo"` when labelled).
- `kind` — `"assert"` / `"assume"` / `"cover"`.
- `formula` — the mu-calc string the engine actually evaluated. Round-trip-parseable via `mu_calculus::parser::parse`.
- `outcome` — one of the four canonical `PropertyVerdict` values. This is the field to gate on.
- `detail` — human-facing extra: `"cube: <false> false / <unknown> unknown"` for cube verdicts; `"skip-<reason>"` for skips (see the skip-reason catalog below). Optional.
- `seeded_predicates` — the abstraction-cube atom strings seeded for this property. Empty for engines that don't use a cube.
- `counterexample` — see [Counterexample](#counterexample--cexcellview--counterexampleview) below. Only present for `Violated` on bare `AF p` / `AG EF p` / `AG AF p` decided by the exact-symbolic engine; `null`/absent otherwise.

## Counterexample — CexCellView / CounterexampleView

Rust source: [`CounterexampleView`](../../crates/mununu-core/src/api/models.rs#L1763) + [`CexCellView`](../../crates/mununu-core/src/api/models.rs#L1777).

Two shapes co-exist in one field:

**(1) Stall-lasso / trap-path** — for liveness/recoverability failures. Reset → `prefix` → repeating `cycle`; each state is an ordered list of register cells.

```jsonc
{
  "prefix": [
    [ { "register": "st_q",     "value": 0 }, { "register": "cnt_q", "value": 0 } ],
    [ { "register": "st_q",     "value": 1 }, { "register": "cnt_q", "value": 1 } ]
  ],
  "cycle": [
    [ { "register": "st_q",     "value": 3 }, { "register": "cnt_q", "value": 5 } ]
  ]
}
```

**(2) Unreachable-target witness** — for a `Violated` bare `EF p` (reachability). The target atoms are simply never reached from reset; no state trace.

```jsonc
{
  "prefix": [],
  "cycle":  [],
  "unreachable_target": ["st_q == 7", "err_q == 1"]
}
```

`unreachable_target` is omitted (or empty) for shape (1). `prefix`/`cycle` are empty for shape (2). Consumers should treat either `unreachable_target.length > 0` or `cycle.length > 0` as valid — never both non-empty.

## Model diagnostics + notes

**`diagnostics`** — one object per response:

- `state_register_count` — total state register lines in the lifted BTOR2.
- `blackboxed_modules` — modules instantiated with no body (cut to free inputs). A non-empty list often explains a `skipped` verdict.
- `gated_resets` — reset inputs the run pinned inactive at model level (`"<signal>=<value>"`), with their `disable iff` guards dropped from the formulas.
- `auto_provided_stubs` — cut flop primitives (e.g. `prim_sparse_fsm_flop`) for which a behavioural stub was auto-injected so the register survives the lift.

**`notes`** — provenance / scope / soundness paragraph objects. Each has:

- `kind` — kebab-case category, e.g. `"config-concretization"`, `"antecedent-shadow"`, `"parameter-override"`, `"control-slice"`, `"lift-frontend"`.
- `level` — `"info"` | `"scope-caveat"` | `"soundness-caveat"`.
- `summary` — one-line human line.
- `detail` — longer explanation of the why + implication.
- `items` — structured operands (e.g. `["cfg_detect_timer_i=7"]`).

Every verdict-modifying decision (config pin, parameter override, cut point, reset gating, antecedent shadow, etc.) is echoed in `notes`, so verdict provenance is explicit.

## Unsupported assertions

Rust source: [`UnsupportedAssertionView`](../../crates/mununu-core/src/api/models.rs#L1551). One entry per SVA that could not be translated:

```jsonc
{ "name": "some_property", "kind": "assert", "reason": "unbounded ##[m:$] delay in sequence antecedent" }
```

Unsupported assertions are **surfaced honestly**, never silently dropped. Reasons are terse; the SVA fragment is documented at [`docs/verifying-rtl.md`](../verifying-rtl.md).

## DiscoveredAssumption — refinement-lane fields

`sv verify-auto` doesn't emit `holds_under` today — that field is populated by the recoverability verb's `--discover-assumptions` flow (see [`crates/mununu-core/src/verdict.rs`](../../crates/mununu-core/src/verdict.rs#L142)). Documented here for completeness because both fields share the `PropertyVerdict` vocabulary:

```jsonc
{
  "phi":         "GF(en == 1)",             // human-readable predicate over named inputs
  "kind":        "input_fairness",          // input_hold | input_conjunction | input_schedule
                                            // reset_eventually | env_strategy
                                            // input_fairness | input_fairness_conjunction
  "non_vacuous": true,                      // gate: does `good` actually get reached under phi?
  "engine":      "exact-symbolic per-cell"  // provenance
}
```

**Critical semantic:** a discovered assumption is a **CONDITIONAL** result. The canonical `PropertyVerdict` remains `unknown` even when a non-vacuous `phi` is found. Consumers must not read `holds_under.length > 0` as "the property holds"; it means "the property would hold under `phi`, and we still don't know unconditionally."

## Stability contract

- **`PropertyVerdict` values** (`"holds"` / `"violated"` / `"unknown"` / `"skipped"`) are **stable**. Adding a new variant would be a breaking change and requires a policy-mandated consumer briefing.
- **`SvVerifyAutoResponse` field shape** is **additive-evolving**: new optional fields land as `#[serde(default)]` (no consumer action required); renaming or removing a field requires a briefing.
- **`PropertyVerdictView` field shape** is **additive-evolving**. Same additive rule.
- **`kind` values on `VerificationNoteView`** are **evolving**: new categories land regularly (each landing carries a note-kind description in a briefing); consumers must treat unknown `kind` as informational (do not error on unknown kinds).
- **`level` values on `VerificationNoteView`** are **stable**: `"info"` | `"scope-caveat"` | `"soundness-caveat"`.
- **`AssumptionKind` values** are **evolving-adding** (each new variant lands with a briefing). Serialized as `snake_case`.
- **Counterexample two-shape polymorphism** (stall-lasso vs unreachable-target) is **stable**: `prefix`/`cycle` for lasso, `unreachable_target` for reachability.

Anything under `notes[].detail` or `notes[].summary` is human-facing prose — **do not string-match against it**. Use `notes[].kind` + `notes[].items` for structured signals.

## Consumer guidance

**For a CI gate**, the minimal parse is:

```js
const bad = report.properties.filter(p => p.outcome === "violated");
process.exitCode = bad.length > 0 ? 2 : 0;
```

Or with the mununu-native semantic — treat both `violated` and `unknown` as failures unless you know you're OK with unknowns:

```js
const bad = report.properties.filter(p => p.outcome === "violated" || p.outcome === "unknown");
```

**For a differential oracle** across two mununu invocations (e.g. shadow-synth on/off): compare `properties[i].outcome` per property. Verdicts that flip between `holds` and `violated` are a soundness alarm; verdicts that gain a definite value from `unknown`/`skipped` are precision gains.

**For a downstream that ingests a stream of verdicts:** always guard against unknown `kind`/`level`/enum values with a default branch. mununu's stability contract permits additive expansion; consumers that error on unknown enums will fail-open on the additive cases.

**For version pinning:** the `Cargo.toml` version is not currently a semantic-version stability commitment. Every downstream that depends on the wire format should either:
- Pin a specific mununu binary version and refresh on a schedule, OR
- Subscribe to consumer briefings under [`../consumer-briefings/`](../consumer-briefings/) and adopt each briefing on its own cadence.

**For error handling:** an HTTP 400 from the API means the request itself was rejected (malformed source, bad flag value); the response body is a `{ "error": string }` shape, not `SvVerifyAutoResponse`. An HTTP 500 means an unexpected engine failure; retry with the same input is usually safe.

## Related

- [`docs/verifying-rtl.md`](../verifying-rtl.md) — user-facing prose on the verify surfaces.
- [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md) — when a wire-format change requires a consumer briefing.
- [`docs/design/antecedent-shadow-synthesis.md`](../design/antecedent-shadow-synthesis.md) — how a `skipped` verdict can flip to a definite one after enabling shadow-synth (or vice versa on `--no-antecedent-shadow`).
- Consumer briefings that touched this surface:
  - [`../consumer-briefings/2026-08-antecedent-shadow-synth.md`](../consumer-briefings/2026-08-antecedent-shadow-synth.md)
  - [`../consumer-briefings/2026-08-multi-atom-antecedent-shadow.md`](../consumer-briefings/2026-08-multi-atom-antecedent-shadow.md)
  - [`../consumer-briefings/2026-08-no-antecedent-shadow-flag.md`](../consumer-briefings/2026-08-no-antecedent-shadow-flag.md)
  - [`../consumer-briefings/2026-08-sv-mutate-param-api-parity.md`](../consumer-briefings/2026-08-sv-mutate-param-api-parity.md)
