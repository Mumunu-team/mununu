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
| which property this is | `properties[].label` (falls back to `.name`) | `properties[].name` alone — it is positional |
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
4. [Trace](#trace)
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

Rust source: [`SvVerifyAutoResponse`](../../crates/mununu-core/src/api/models.rs#L1882).

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

Rust source: [`PropertyVerdictView`](../../crates/mununu-core/src/api/models.rs#L2393).

```jsonc
{
  "name":               "my_module_sva_0",    // POSITIONAL; re-points on an insertion
  "label":              "my_assertion",        // the SV label; absent when unlabelled
  "kind":               "assert",              // "assert" | "assume" | "cover"
  "formula":            "nu X. ((!req || [] grant) && [] X)",  // as written for an `ltl` annotation row; else the mu-calc lift
  "formula_language":   "mu-calculus",         // "mu-calculus" | "ltl" — how to read `formula`
  "translated":         "...",                  // the mu-calculus the engine decided; present only on an `ltl` row
  "outcome":            "holds",               // canonical PropertyVerdict as-string
  "detail":             "cube: 0 false / 12 unknown", // optional; skip reason, cell counts
  "seeded_predicates":  ["req == 1", "grant == 1"],   // atoms auto-seeded for this property
  "trace":              null                   // a Trace on a violated row that carries a path
}
```

**Field-by-field:**

- `name` — **positional**: `<module>_sva_<index>`, or `ann_guarantee_<index>` for a
  `@mununu_guarantee` annotation. *(Corrected in mununu#544: this field was previously documented
  here as "SVA label from the source", which it never was. A consumer reasonably read that and
  pinned an expectation to the label, which did not resolve.)* An insertion mid-file re-points
  every later name, and the pin still resolves — to a different property. Prefer `label`.
- `label` — mununu#544 — the assertion's **SV label**, when it has one (`a_reseed_only_at_a_span_edge:
  assert property …` → `"a_reseed_only_at_a_span_edge"`). This is what the author wrote, so a
  reorder is a no-op and a rename is a hard error. Absent for an unlabelled assertion and for
  every `@mununu_guarantee` annotation. **`--expect NAME=VERDICT` accepts either**, label first.
- `kind` — `"assert"` / `"assume"` / `"cover"`.
- `formula` — the property as its author wrote it when that was not mu-calculus (an `@mununu_guarantee ltl …` row shows its LTL, 2026-10-29); otherwise the mu-calc string the engine actually evaluated, round-trip-parseable via `mu_calculus::parser::parse`.
- `formula_language` — `"mu-calculus"` | `"ltl"`: how to read `formula`.
- `translated` — the mu-calc string the engine evaluated, present only when `formula` is written in another language (an `ltl` row); absent otherwise. `translated ?? formula` is always mu-calculus.
- `outcome` — one of the four canonical `PropertyVerdict` values. This is the field to gate on.
- `detail` — human-facing extra: `"cube: <false> false / <unknown> unknown"` for cube verdicts; `"skip-<reason>"` for skips (see the skip-reason catalog below). Optional.
- `seeded_predicates` — the abstraction-cube atom strings seeded for this property. Empty for engines that don't use a cube.
- `trace` — see [Trace](#trace) below. Present on a `violated` row that carries a path (safety via native BMC, liveness lassos, trap paths, unreachable `EF` targets); absent otherwise.

## Trace

Rust source: [`Trace`](../../crates/mununu-core/src/api/models.rs) + [`CexCellView`](../../crates/mununu-core/src/api/models.rs). The one counterexample shape since 0.5, on every verb that returns a path (`sv verify-auto` rows, `btor2 verify` with `--witness` / a fault `--target`, cegar, the `btor2 game` starvation lasso):

```jsonc
{
  "kind": "lasso",                 // finite | lasso | trap | unreachable | attack
  "steps": [
    { "cycle": 0, "state":  [ { "register": "st_q", "value": 0, "hex": "0x0", "bits": 2 } ],
                  "inputs": [ { "register": "req",  "value": 1, "hex": "0x1", "bits": 1 } ] },
    { "cycle": 1, "state":  [ { "register": "st_q", "value": 1, "hex": "0x1", "bits": 2 } ] }
  ],
  "loop_start": 1,                 // lasso only: the step the last one loops back to
  "unreachable_target": [],        // unreachable only: the atoms no path reaches
  "sources": [                     // the legend: where each cell the trace shows lives in the RTL
    { "cell": "state_q", "kind": "register",
      "declared": { "file": "pump_pair.sv", "line": 32 },
      "assigned": [ { "file": "pump_pair.sv", "line": 52 } ] },
    { "cell": "flow", "kind": "input", "declared": { "file": "pump_pair.sv", "line": 25 } }
  ],
  "replay": {                      // when a Verilator testbench was requested
    "testbench": { "tb_cpp": "…", "makefile": "…", "checks": 16, "skipped": [] },
    "gtkw":   "…",                  // replay.gtkw: the GTKWave save file for the trace's signals
    "dir":    "tb/alternates",      // CLI only: where the files were written
    "result": {                     // --replay / replay: true — the bench built and run
      "outcome": "match",           // "match" | "mismatch" | "unavailable"
      "reason": null,               // unavailable: Verilator or make missing, a compile error
      "checks": 16, "mismatches": 0,
      "first_mismatch": null,       // mismatch: {cycle, register, got, expected}
      "assertions": [],             // Verilator's `Assertion failed` lines on the final state
      "vcd_bytes": 2037,            // replay.vcd, when `make trace` wrote it
      "verilator": "5.0xx",
      "log": "MATCH\nwrote replay.vcd"
    }
  }
}
```

- `sources` — the legend (2026-10-29): the first step's registers then its inputs, each with its
  declaration (from slang's AST on every front end, or yosys's netlist attribute when present) and,
  for a register, the `always` block(s) driving it (from the netlist; absent when sv2v preprocessed
  the source); `diagnostics.source_map.status` is `available`, `partial` (with what is missing in
  `reason`) or `unavailable`; the key is omitted when nothing could be placed.
- `replay` — present when `emit_testbench` / `--emit-testbench` was requested on a row with a path:
  `testbench` (the files), `gtkw` (the GTKWave save file), `dir` (CLI only) and, under `replay: true` /
  `--replay`, `result` — the simulator's verdict, with `outcome: "unavailable"` and a `reason` when
  the bench could not run (2026-10-29).

- `finite` — reset → the failing step (a safety violation). `lasso` — a prefix and a cycle repeated forever (a liveness stall). `trap` — reset → a closed trap the target is never reached from again (a refuted `AG EF` decided by the trap certificate). `unreachable` — no steps; `unreachable_target` lists the atoms. `attack` — one step per cycle with the fault injections on `steps[].faults` (`{site, value, hex}`); the adversary records injections, not states.
- `state` / `inputs` are register cells: `value` when it fits 64 bits (else `null`), `hex` and `bits` always. A CTXDSL step names its state in `state_name` and the transition's labels in `labels`.
- A cegar counter-trace lists each step's predicates as 1-bit cells; a game starvation lasso's cells carry the value's bit length in `bits` (the lasso records no widths).

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

Rust source: [`UnsupportedAssertionView`](../../crates/mununu-core/src/api/models.rs#L1654). One entry per SVA that could not be translated:

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
- [`docs/design/antecedent-shadow-synthesis.md`](../design/antecedent-shadow-synthesis.md) — history: the shadow synthesis was removed in mununu#677 (it checked a `|=>` consequent one cycle late); a `skipped` verdict on an input-reading atom flips to a definite one by inputs-in-the-state, and `no_input_state: true` restores the refusal. `no_antecedent_shadow` is accepted and ignored.
- Consumer briefings that touched this surface:
  - [`../consumer-briefings/2026-08-antecedent-shadow-synth.md`](../consumer-briefings/2026-08-antecedent-shadow-synth.md)
  - [`../consumer-briefings/2026-08-multi-atom-antecedent-shadow.md`](../consumer-briefings/2026-08-multi-atom-antecedent-shadow.md)
  - [`../consumer-briefings/2026-08-no-antecedent-shadow-flag.md`](../consumer-briefings/2026-08-no-antecedent-shadow-flag.md)
  - [`../consumer-briefings/2026-08-sv-mutate-param-api-parity.md`](../consumer-briefings/2026-08-sv-mutate-param-api-parity.md)
