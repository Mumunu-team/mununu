# BTOR2 + SV verb responses — narrative

> Source of truth: [`crates/mununu-core/src/api/models.rs`](../../crates/mununu-core/src/api/models.rs) — surface: API. Machine-readable schemas live alongside as [`*.schema.json`](.) files; this document is the prose you read *before* wiring a consumer to them.

The BTOR2-direct property verbs (`verify`, `verify-liveness`, `verify-liveness-all`, `verify-recoverability`, `check-fsm`) and their SV-wrapped siblings (`sv/verify`, `sv/verify-liveness`, `sv/verify-liveness-all`, `sv/verify-recoverability`, `sv/check-fsm`) all end in the same canonical `PropertyVerdict` vocabulary — `holds` / `violated` / `unknown` / `skipped`. What differs across verbs is *which structured detail* accompanies that verdict.

## The axis map

| Verb pair | Property class | Response type | Detail axis |
|-----------|---------------|---------------|-------------|
| `btor2 verify` / `sv verify` | Safety (`bad` unreachable) | `Btor2VerifyResponse` | Per-engine breakdown, soundness-alarm flag |
| `btor2 verify-liveness` / `sv verify-liveness` | Response `AG(a → AF b)` | `Btor2VerifyLivenessResponse` | Reduced-property echo, deciding engines |
| `btor2 verify-liveness-all` / `sv verify-liveness-all` | Conjunctive `⋀ᵢ AG(aᵢ → AF bᵢ)` | `Btor2VerifyLivenessResponse` | Same as `-liveness` — one merged verdict |
| `btor2 verify-liveness-under-fairness` (mununu#477) | Response under fairness `(⋀ⱼ GF fairⱼ) → AG(a → AF b)` | `Btor2VerifyLivenessResponse` | Same shape as `-liveness`; empty `fairness` reduces to `-liveness` exactly |
| `btor2 verify-recoverability` / `sv verify-recoverability` | Recoverability `AG EF good` | `Btor2VerifyRecoverabilityResponse` | Optional `VerdictRefinement` tree (vacuous / config-partition / holds-under / ⊥-hint) |
| `btor2 check-fsm` / `sv check-fsm` | Auto-scan of illegal FSM encodings | `Btor2CheckFsmResponse` | Per-register findings + counts |

The SV wrappers exist so downstream consumers do not need the intermediate `sv emit-btor2` step. They accept SystemVerilog on the wire, lift it via `sv2v` + Yosys (or `read_slang` when `use_slang` is set — the reliable path for modern SV per `docs/verifying-rtl.md`), and return the sibling BTOR2 response shape.

## `POST /btor2/verify` (and `POST /sv/verify`) — safety portfolio

Schema: [`btor2-verify-response.schema.json`](btor2-verify-response.schema.json)

Runs every available sound reach-portfolio engine (native BMC / k-induction, McMillan interpolation, in-process Boolector when the feature is on, and any external members the binary can locate) against the design's `bad` signal.

```json
{
  "verdict": "holds",
  "reachable_by": [],
  "unreachable_by": ["native_kind", "mcmillan_interp"],
  "contradiction": false
}
```

- `verdict` — the merged canonical answer.
- `reachable_by` / `unreachable_by` — the deciding engines. An `unreachable_by` list of length ≥ 1 is a sound safety proof; a `reachable_by` list of length ≥ 1 is a real counterexample.
- `contradiction` — TRUE only when two sound engines disagree. **Treat this as a soundness alarm**, not a verdict-selection hint: raise it to a human and cross-check the input BTOR2 rather than picking a side.

## `POST /btor2/verify-liveness-under-fairness` — response properties under a fairness assumption (mununu#477 Option B)

Schema: [`btor2-verify-liveness-under-fairness-request.schema.json`](btor2-verify-liveness-under-fairness-request.schema.json) (response reuses `btor2-verify-liveness-response.schema.json`)

Decides `(⋀ⱼ GF fairⱼ) → AG(request → AF grant)` via the Emerson–Lei fair-cycle extension of the plain l2s. The BTOR2 monitor gains one `fairⱼ_seen` latch per fairness atom (each mirroring the existing `b_seen`); `bad = looped ∧ ¬b_seen ∧ ⋀ⱼ fairⱼ_seen`. A reachable `bad` ⇒ a lasso exists that satisfies EVERY fairness constraint AND leaves a request forever ungranted ⇒ VIOLATED. An unreachable `bad` ⇒ HOLDS. Empty `fairness` recovers `verify-liveness` exactly.

Request example:

```json
{
  "content": "<btor2 source>",
  "request": "req == 1",
  "grant": "ack == 1",
  "fairness": ["grant_cpu == 1"]
}
```

Response:

```json
{
  "verdict": "holds",
  "property": "(GF (grant_cpu == 1)) -> AG((req == 1) -> AF (ack == 1))",
  "decided_by": ["native_kind"]
}
```

- `property` echoes the fully-quantified formula for provenance (empty fairness → the plain `AG(...) -> AF(...)` shape).
- Soundness guarantee: the fair-cycle l2s is sound + complete for the response shape (see [`crates/mununu-core/src/adapter/btor2/l2s_monitor.rs`](../../crates/mununu-core/src/adapter/btor2/l2s_monitor.rs) module docs for the construction and Emerson–Lei argument). A useless fairness atom that env satisfies trivially (e.g. `GF (dead_input == 0)` where `dead_input` is not wired to anything) does NOT rescue a genuinely-starving design — validated by the `fair_gated_is_not_rescued_by_useless_fairness` soundness control.

## `POST /btor2/verify-liveness` and `-liveness-all` — response properties

Schema: [`btor2-verify-liveness-response.schema.json`](btor2-verify-liveness-response.schema.json) (shared)

Decides `AG(request → AF grant)` via the standard l2s (liveness-to-safety) reduction, then hands the reduced safety query to the reach portfolio. The `-all` variant does the same for a conjunction `⋀ᵢ AG(aᵢ → AF bᵢ)`.

```json
{
  "verdict": "unknown",
  "property": "AG((st == 1) -> AF (st == 3))",
  "decided_by": []
}
```

- `property` — the reduced formula, echoed for provenance. On the `-all` verb this is the AND of every response's reduction.
- `decided_by` — engines that decided the reduced `bad`-reachability query.

`unknown` here typically means the l2s reduction pushed the bit-width past the portfolio's cap; try `--engine` overrides on the CLI or narrow the atoms.

## `POST /btor2/verify-recoverability` (and `POST /sv/verify-recoverability`) — the deepest response

Schema: [`btor2-verify-recoverability-response.schema.json`](btor2-verify-recoverability-response.schema.json)

Decides `AG EF good` — "from every reachable state, can the design get back to `good`?". This is the recoverability property SVA cannot express, and the one where mununu's structured refinement earns its keep.

```json
{
  "verdict": "unknown",
  "property": "AG EF (state_q == 3)",
  "refinement": {
    "vacuous": null,
    "config_partition": {
      "config_atoms": [["mode", 2]],
      "holds": [[["mode", 0]], [["mode", 1]]],
      "violated": [],
      "unknown": [[["mode", 2]]],
      "vacuous": [],
      "exhaustive": true,
      "engine": "exact-symbolic per-config pin"
    },
    "holds_under": [],
    "bot_diagnosis": {
      "uncertified_counters": [["retry_ctr", 8]],
      "wide_influences": []
    }
  }
}
```

- `verdict` — the CANONICAL (unconditional) answer. A `config_partition` or a non-empty `holds_under` never changes this — a refinement is a diagnostic, not a verdict.
- `property` — the checked formula echoed for provenance.
- `refinement` — present only when the request set `refine`, `config_values`, or `discover_assumptions`. Structured detail:
  - `vacuous` — the recovery target `good` was never reachable from the initial state. Bare `AG EF good` is degenerate then; the plain verdict is misleading.
  - `config_partition` — per-config decided verdicts when `config_values` was requested, every named value included: a single value (`"rst_n=1"`) is a scoped pin, reported as its one cell (mununu#634). Each row is `[(name, u64)]`. `exhaustive: true` means the enumerated cells cover the entire reachable config space. The bare `refine` auto-partitions over the detected reset and reports only a partition the verdict genuinely depends on (two or more non-empty cells).
  - `holds_under` — CONDITIONAL results: the property holds under environment assumption `φ`. `kind` names the shape (`InputHold`, `InputConjunction`, `InputFairness`, …). `non_vacuous: true` means the non-vacuity gate passed — `good` is actually reached under `φ`, not just trivially satisfied.
  - `bot_diagnosis` — best-effort structural "why ⊥" hint. `uncertified_counters` names counters gating recovery without a ranking certificate; `wide_influences` names wide state/inputs the predicate cube cannot enumerate.

The full refinement tree includes `AssumptionKind`, `VerdictRefinement`, `VacuityWitness`, `ConfigPartition`, `DiscoveredAssumption`, and `RecoverabilityBotDiagnosis`; consult the JSON schema for their exact shape.

## `POST /btor2/check-fsm` (and `POST /sv/check-fsm`) — auto FSM scan

Schema: [`btor2-check-fsm-response.schema.json`](btor2-check-fsm-response.schema.json)

Auto-discovers the design's FSM-like state registers (state registers narrower than `max_width`, default 6 bits), computes each one's legal encoding set from the design's own next-state logic, and reports whether any illegal encoding is reachable — **without the caller naming any property**.

```json
{
  "fsm_registers_checked": 1,
  "illegal_encodings_found": 1,
  "registers": [
    {
      "register": "ctrl_state",
      "kind": "fsm",
      "legal_encodings": [0, 1, 2, 3],
      "verdict": "violated",
      "illegal_encoding_reachable": true
    },
    {
      "register": "retry_cnt_q",
      "kind": "counter",
      "legal_encodings": [],
      "verdict": "skipped",
      "illegal_encoding_reachable": false
    }
  ]
}
```

- `verdict` per register is the same canonical vocabulary. `illegal_encoding_reachable` is a convenience alias for `verdict == "violated"`.
- `register` is the cell's own symbol or a **value-identical** alias of it (`state_q`, the port that mirrors it) — never a next-value wire such as `beat_cnt_d` ([`parser::canonical_register_names`](../../crates/mununu-core/src/adapter/btor2/parser.rs); mununu#633).
- `kind` (mununu#633): `"fsm"` is an enumeration-valued register the scan checked; `"counter"` is a register its own next-state logic steps by ±1 (`cnt + 1`, `cnt - 1`) — its values are a range, not an enumeration, so it is listed with no `legal_encodings` and `"skipped"`, and does not count toward `fsm_registers_checked`. Arithmetic by any other step (`st + 3`) stays `"fsm"`: a computed out-of-enum value is the bug class the scan exists for.
- Wider registers are treated as datapath and skipped (see `default_fsm_max_width` in the source).

## The SV request shapes

All five SV wrappers share the same lift-input core:

- `source` — the primary SystemVerilog module content.
- `additional_sources` — extra packages / includes as `FileContent` records.
- `top` — top module for the lift (auto-detected when omitted).
- `use_sv2v` — run `sv2v` before Yosys (needed for modern SV constructs Yosys refuses).
- `use_slang` — force the `read_slang` frontend (needed for `while` loops, `import pkg::*;`, and other constructs `sv2v` silently drops — see the CLAUDE.md "slang-gated" note under `SVA-verification e2e validation`).

Verb-specific fields (`request` / `grant` / `target` / `responses` / `max_width` / `predicates` / `refine` / `config_values` / `discover_assumptions`) match the BTOR2 sibling's non-`content` fields one-for-one. A downstream consumer that already parses the BTOR2 verbs adds the SV wrappers cheaply.

## Consumer guidance

Same as [`verdict.md`](verdict.md):

- Treat `verdict` as the source of truth; parse structured detail only when the verb ships it.
- Tolerate unknown enum-shaped strings (`AssumptionKind`, `verdict`, `terminated_with`). mununu's stability contract allows additive expansion.
- A `contradiction: true` on `/btor2/verify` is a soundness alarm, not a hint; escalate rather than picking a side.
- `refinement` on `/btor2/verify-recoverability` is optional. Only ask for it (`refine: true`, `config_values: […]`, or `discover_assumptions: true`) when the consumer knows what to do with it — the default path stays minimal.

## Not covered here

- **`POST /btor2/cegar`** and **`POST /sv/cegar`** — the CEGAR refinement-trace endpoint (viewer-shaped). Follows in a subsequent PR alongside `/context/synthesize` and `/synth/gr1`.
- **`POST /sv/verify-auto`** — the flagship BTOR2-free entry with its own richer response tree. Covered in [`verdict.md`](verdict.md).

## See also

- [`README.md`](README.md) — index of every shipped schema file plus the update procedure.
- [`verdict.md`](verdict.md) — narrative for the flagship `sv verify-auto` response.
- [`../verifying-rtl.md`](../verifying-rtl.md) — user-facing walkthrough of each verb from the CLI side.
- [`../policies/cross-repo-impact.md`](../policies/cross-repo-impact.md) — when a wire-format change must ship a briefing.
