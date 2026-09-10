# Verifying RTL with Mununu — the property verbs, no-sidecar SV, and agent integration

This page answers three practical questions:

1. **What properties can I check, and how?** — the three property *verbs* over a
   BTOR2 design, all speaking one verdict vocabulary.
2. **Can I verify a SystemVerilog module with no hand-written setup?** — yes,
   `sv verify-auto` extracts the design's own assertions and checks them, no sidecar.
3. **Can an external tool (e.g. an agent writing RTL) drive this over HTTP to check
   its generated designs?** — yes, with three honest caveats spelled out below.

Every verdict below is one of the canonical labels `holds` / `violated` / `unknown`
/ `skipped`.

> Source of truth: [`PropertyVerdict`](../crates/mununu-core/src/verdict.rs#L22) — surface: (CLI+API+UI)

`holds` and `violated` are **definite** (sound within the engine's cap / abstraction);
`unknown` is an honest abstention (over-cap, timeout, or an undecided abstraction);
`skipped` means the property was not evaluated (out of the supported fragment).

---

## The property verbs (BTOR2-direct)

Three commands decide the three property classes over a BTOR2 file. Each ships on
CLI (`mununu btor2 <verb>`), HTTP API (`POST /api/v1/btor2/<verb>`), and the UI
client. They take a BTOR2 design plus, where needed, an explicit atom naming the
signal/value of interest (`"state_q == 3"`).

### `btor2 verify` — safety / reachability

> Source of truth: [`decide_reach_portfolio_parallel`](../crates/mununu-core/src/adapter/reach_portfolio.rs#L260) — surface: (CLI+API+UI)

Decides `bad`-reachability with the multi-engine safety portfolio — the exact BDD
engine, the in-house native BMC + k-induction and SPACER (IC3/PDR) engines (all
in-process), plus the `btormc` and `Pono` subprocess members when present, and a
**last-resort** in-house interpolation engine (owned McMillan forward reachability;
[`native_interp`](../crates/mununu-core/src/adapter/btor2/native_interp.rs)) invoked
only when every other member abstains — merged
under a **differential-oracle** discipline: the first definite verdict wins, and two
sound engines disagreeing raises a `contradiction` soundness alarm rather than a
guess. `verdict` is the property reading (`bad` unreachable = `holds`, reachable =
`violated`); the per-engine `reachable_by` / `unreachable_by` breakdown and the
`contradiction` flag carry the detail.

```bash
mununu btor2 verify design.btor2
```

### `btor2 verify-liveness` — response liveness `AG(request → AF grant)`

> Source of truth: [`response_liveness_rescue_atoms`](../crates/mununu-core/src/adapter/liveness_rescue.rs#L239) — surface: (CLI+API+UI)

Decides the canonical request/grant property — "whenever `request` holds, `grant` is
eventually reached on **every** path" — by a sound liveness-to-safety reduction
(Biere–Artho–Schuppan): a `pending` latch, a nondeterministic snapshot of the state,
and loop-closure detection turn "a reachable cycle leaves a request forever
ungranted" into a single `bad`-reachability query the portfolio decides at scale.
Cross-checked against the exact engine.

```bash
mununu btor2 verify-liveness design.btor2 --request "st == 1" --grant "st == 2"
```

### `btor2 verify-liveness-under-fairness` — response liveness under a `GF` fairness assumption (mununu#477)

> Source of truth: [`response_liveness_rescue_under_fairness`](../crates/mununu-core/src/adapter/liveness_rescue.rs#L313) — surface: (CLI+API+UI)

Decides `(⋀ⱼ GF fairⱼ) → AG(request → AF grant)` via the Emerson–Lei extension of the plain l2s: one `fairⱼ_seen` latch per fairness atom (each mirroring the existing `b_seen`), conjuncted into `bad = looped ∧ ¬b_seen ∧ ⋀ⱼ fairⱼ_seen`. A reachable `bad` ⇒ a lasso exists that satisfies every fairness constraint AND leaves a request forever ungranted ⇒ VIOLATED. Empty `--fairness` recovers `verify-liveness` exactly, byte-for-byte on the emitted monitor. Sound + complete for the response shape (a useless fairness atom env satisfies trivially does NOT rescue a genuinely starving design — the `fair_gated_is_not_rescued_by_useless_fairness` soundness control validates this).

Use when a block cannot own its liveness on its own — its grant comes from an arbiter it does not control — and you can express the arbiter's obligation as a `GF <signal>` on a primary input. See [`design/emerson-lei-fair-cycle.md`](design/emerson-lei-fair-cycle.md) for the construction and soundness argument (module-level docs in [`crates/mununu-core/src/adapter/btor2/l2s_monitor.rs`](../crates/mununu-core/src/adapter/btor2/l2s_monitor.rs)).

```bash
mununu btor2 verify-liveness-under-fairness design.btor2 \
    --request "req == 1" --grant "ack == 1" \
    --fairness "grant_cpu == 1"

# Multiple fairness constraints — conjunctive `GF fair_1 ∧ GF fair_2` — repeat --fairness.
mununu btor2 verify-liveness-under-fairness design.btor2 \
    --request "req == 1" --grant "ack == 1" \
    --fairness "fair_1 == 1" --fairness "fair_2 == 1"
```

### `btor2 verify-recoverability` — recoverability `AG EF good`

> Source of truth: [`verify_recoverability`](../crates/mununu-core/src/adapter/recoverability.rs#L46) — surface: (CLI+API+UI)

Decides "from every reachable state, can the design still get **back** to a good
state?" — the CTL formula `AG EF good = ν Y. ((μ X. (good ∨ ◇X)) ∧ □Y)`. This is an
alternating fixpoint: the `◇` (some-successor) inside the `□` (all-successors) is
branching content that **SVA / LTL cannot express** (see
[`recoverability-vs-sva.md`](design/recoverability-vs-sva.md)). A `violated` verdict
means a reachable state is a trap from which `good` is unreachable. Decided by the
exact 3-valued engine (sound at every alternation depth, definite within its 40-bit
cone cap; `unknown` over the cap).

```bash
mununu btor2 verify-recoverability design.btor2 --target "state_q == 3"
```

For designs wider than the exact engine's cap, the same `AG EF` formula scales via
predicate abstraction on the `btor2 cegar … --must-edge-inference smt-hyper-must`
path (validated on real OpenTitan `csrng` RTL — see `recoverability-vs-sva.md` §3.2).

---

### `sv verify-auto` fair-cycle bridge — `// @mununu_assume GF <atom>` (mununu#477)

> Source of truth: [`escalate_bottom`](../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: (CLI+API+UI)

A `// @mununu_assume GF <REG op VALUE>` inline annotation is **auto-applied to any response-shape guarantee** (`AG(a → AF b)`) in the same source. The SV verify-auto orchestration parses the `GF <atom>` into a typed fairness atom during annotation scanning; when a response-shape guarantee's cube-abstraction verdict is ⊥, the ⊥-escalation router dispatches through [`response_liveness_rescue_under_fairness`](../crates/mununu-core/src/adapter/liveness_rescue.rs) (the fair-cycle l2s primitive described above) instead of the plain `response_liveness_rescue`. A definite `holds` under fairness means the guarantee holds against every environment schedule satisfying the assumption infinitely often; a definite `violated` is a reachable lasso satisfying the assumption that still leaves the request forever ungranted.

Multiple `// @mununu_assume GF <atom>` in one source accumulate as a conjunction `(⋀ⱼ GF fairⱼ)`. The scanner records the applied assume in the `annotation-properties` verification note; each fair-cycle rescue emits a `fair-cycle-rescue` note naming the assumption(s) applied.

Assumes that don't fit `GF <REG op VALUE>` — a state predicate, a non-response guarantee shape, or coupled multi-guarantee fairness — remain recorded rather than auto-applied. Route those through `mununu btor2 verify-liveness-under-fairness` on the emitted BTOR2, or model in CTXDSL where the GR(1) game engine handles multi-pair coupled fairness directly.

```systemverilog
// @mununu_assume GF grant_cpu == 1
// @mununu_guarantee nu Z. ((!(req == 1) || mu Y. ((ack == 1) || [] Y)) && [] Z)
module wb_mem_client(...);
    // ...
endmodule
```

```bash
# The guarantee is decided under GF(grant_cpu == 1) automatically:
mununu sv verify-auto wb_mem_client.sv
```

## No-sidecar SystemVerilog verification: `sv verify-auto`

> Source of truth: [`verify_auto`](../crates/mununu-core/src/adapter/slang/verify_auto.rs#L1537) — surface: (CLI+API+UI)

`sv verify-auto` takes a SystemVerilog module and checks its properties with **no
hand-written sidecar and no supplied formula**. End to end:

1. **slang** elaborates the source and extracts the design's own SVA `assert` /
   `assume` / `cover` properties, translating each to mu-calculus.
2. **sv2v → Yosys** lift the RTL to a BTOR2 model.
3. Cube predicates are **auto-seeded from each property's atoms** (no sidecar to
   author) and the CEGAR loop refines on an undecided verdict.
4. Each property gets a `holds` / `violated` / `unknown` / `skipped` verdict.

The HTTP request is just the source plus options — there is no `sidecar` or `formula`
field:

> Source of truth: [`sv_verify_auto_handler`](../crates/mununu-core/src/api/handlers.rs#L846) — surface: API

```bash
mununu sv verify-auto my_module.sv --preprocess-sv2v
```

```jsonc
// POST /api/v1/sv/verify-auto
{ "source": "module m(...); ... assert property (...); endmodule",
  "use_sv2v": true,
  "must_edge_inference": "smt-hyper-must" }   // sound νμ verdicts; all fields optional
// → { "properties": [ { "name": ..., "outcome": "holds" | "violated" | ... } ], "unsupported": [...] }
```

The response reports a verdict per translated assertion, plus a list of assertions
that did **not** translate (surfaced honestly, never silently dropped) and
model-level diagnostics.

### The machine-readable report: `--json` (mununu#536)

> Source of truth: [`impl From<&AutoVerifyReport> for SvVerifyAutoResponse`](../crates/mununu-core/src/api/models.rs) — surface: (CLI+API)

**Do not parse the transcript.** `sv verify-auto --json` emits the *same document* the HTTP API returns, described by [`docs/api-schemas/sv-verify-auto-response.schema.json`](api-schemas/sv-verify-auto-response.schema.json) and pinned by a drift test. One shape, both surfaces:

```bash
mununu --quiet sv verify-auto design.sv --source design_sva.sv --top design --json
```

```jsonc
{
  "properties": [
    { "name": "dut_sva_0", "kind": "assert", "formula": "nu X. …", "outcome": "holds",
      "detail": null, "seeded_predicates": ["st_q == 2"] },
    { "name": "dut_sva_1", "kind": "assert", "formula": "nu X. …", "outcome": "unknown",
      "detail": "32768 cell(s)", "unknown_cells": 32768, "seeded_predicates": [] }
  ],
  "unsupported": [ { "name": "dut_sva_2", "kind": null,
                     "reason": "unsupported binary op: BinaryAnd" } ],
  "diagnostics": { "state_register_count": 12, "blackboxed_modules": [], "gated_resets": ["rst_n=1"],
                   "auto_provided_stubs": [], "frontend": "read_verilog + sv2v",
                   "config_values": ["cfg_timer=7"], "cutpoints": [] },
  "notes": [ { "kind": "coverage-summary", "level": "info", "summary": "…", "items": [] } ]
}
```

**`properties[]` is the verdict record. Notes are commentary — never count them.** A note is prose written for a human reading a terminal; `properties[]` is the answer. Concretely: read `outcome` per property, and take the ⊥ figure from `unknown_cells` (or `false_cells` / `skip_reason`) rather than parsing the `detail` sentence that contains it.

```bash
# every property and its verdict
mununu --quiet sv verify-auto design.sv --json | jq -r '.properties[] | "\(.name) \(.outcome)"'

# did anything fail to translate, and why?
mununu --quiet sv verify-auto design.sv --json | jq -r '.unsupported[] | "\(.name): \(.reason)"'

# act on a scope caveat without string-matching prose
mununu --quiet sv verify-auto design.sv --json | jq '[.notes[] | select(.level=="scope-caveat")]'
```

`unsupported[]` carries mununu's own **reason** for every assertion it could not translate (`"unsupported binary op: BinaryAnd"`, `"dynamic bit-select …"`). An assertion that does not appear in `properties[]` is not being checked, and this is where it says why.

### Asserting the verdicts you claim: `--expect-*` (mununu#537)

> Source of truth: [`adapter::slang::expectations::evaluate`](../crates/mununu-core/src/adapter/slang/expectations.rs) — surface: (CLI+API)

A verification gate never wants "did it pass". It wants **"did exactly this happen"**. Three verbs, on both surfaces:

```bash
# every property holds, nothing unsupported/unknown/skipped, and there are EXACTLY 8 of them
mununu sv verify-auto dut.sv --source dut_sva.sv --top dut --expect-all-hold --expect-count 8

# a contrast twin: these must be VIOLATED, and every other property must still HOLD
mununu sv verify-auto faulty.sv --source dut_sva.sv --top dut --expect-violated dut_sva_sva_1

# exact per-property claims; unnamed properties are ignored, but an unnamed VIOLATED still fails
mununu sv verify-auto dut.sv --source dut_sva.sv --top dut \
    --expect 'dut_sva_sva_0=holds,dut_sva_sva_1=unknown'
```

| flag | the failure it exists to catch |
|---|---|
| `--expect-all-hold` | stricter than `--fail-on unknown`: also rejects `skipped` (which the gate treats as a pass) and an assertion that did not translate — both are properties not being checked |
| `--expect-count N` | a binding that stopped binding produces **fewer** properties, and a smaller all-green set reads as a clean pass. Nothing else catches it, because a property that stopped being produced is invisible to every per-property check |
| `--expect-violated a,b` | a twin that breaks *every* property teaches nothing about which property covers which fault, so "everything else still holds" is not optional |
| `--expect 'a=VERDICT,…'` | pins exact verdicts; an **unnamed** VIOLATED is still a failure, so the verb cannot become a way to ignore what you did not mention |

**Exit codes.** Declaring an expectation **supersedes** `--fail-on` — it has to, or `--expect-violated` would exit 2 on the very violation you asked for.

| exit | meaning |
|---|---|
| `0` | expectations met |
| `4` | **a verdict was not what you claimed** |
| `1` | the run failed (tool/usage error) — including a malformed `NAME=VERDICT` |

`2` / `3` remain the ordinary verdict gate for runs that declare no expectations.

#### Pinning a ⊥ is the sound way to record one

`--expect 'sva_1=unknown'` records an abstention as a **claim**, and the claim fails when the property becomes decidable. That is the desired behaviour, not an annoyance: monono's `video_timing` twin pinned `sva_1=UNKNOWN`, mununu#503 made it decidable, and the gate failed on the next run — an upstream improvement surfaced as a signal instead of vanishing into silence.

There is deliberately **no** "tolerate undecided" flag. One was written downstream and deleted: it was introduced for three abstaining assertions that turned out to be **false rather than hard**, and the excuse made the abstention comfortable until they were restated decidably and all nine held. A ⊥ should keep feeling like "not checked".

The API peer is an `expectations` object on the request (`all_hold`, `count`, `violated`, `named`), with the result returned as `expectations: { satisfied, failures[] }` — so an HTTP consumer learns *which* claim failed without parsing prose.

### Shrinking a parameterised design: `--param`

> Source of truth: [`build_chparam_passes`](../crates/mununu-core/src/adapter/yosys/mod.rs) — surface: (CLI+API+UI)

A design whose timing intervals are **parameters** sizes its counters by those
parameters — a 20 000-cycle power-up wait makes a 15-bit counter, and the exact
engine then abstains (`skip-diameter-bound`) even though the properties are about
the *order* of operations, not durations. `--param NAME=VALUE` (API `params: [...]`)
overrides a module parameter **before elaboration**, so the counters get smaller
without a wrapper module (a wrapper would rename the SVA atoms and break binding):

```bash
mununu sv verify-auto sdram_ctrl.sv --top sdram_ctrl --param INIT_WAIT=4
```

`NAME=VALUE` targets the top module; `MODULE.NAME=VALUE` a submodule. The
mechanism is frontend-appropriate — yosys `chparam` on the read_verilog frontend
(parameters stay unresolved until `hierarchy`), slang `-G` on the `--frontend
slang` path (slang elaborates at read time). A parameter that is **not** a real
parameter of the design is a hard **error** (it lists the declared parameters),
never a silent drop; the applied overrides are echoed as a `parameter-override`
scope note — the verdicts are scoped to those values.

`--param` is also the remedy for a **bit-blast size wall**: several wide counters
(e.g. six 32-bit counters, each doubled by its `$past` shadow) can push a
property's state cone past the BDD node budget, so the symbolic engine **abstains**
on that one property with a `skip-bitblast-oom` note (naming the cone's bit width)
rather than crashing the run — the other properties still report. Shrink a width
parameter (`--param BW_WIDTH=4`) so the counters fit, or fall back to
`--engine explicit`. This is a size threshold, not a malformed input.

The exact engine has **four** independent budgets, and an abstention **names which
one it hit and the knob to raise** — `BIT CAP` (`MUNUNU_BDD_MAX_BITS`), `NODE`
budget (`MUNUNU_BDD_ARENA_NODES`, which scales the OxiDD arena), `ITERATION`
budget (`MUNUNU_BDD_ITER_BUDGET`, the μ/ν fixpoint step cap), and `WALL-CLOCK`
(`MUNUNU_BDD_TIME_BUDGET_MS`, default 10 s). Bit count cannot see the reachable
*diameter*, so a cone that fits the bit cap can still abstain on the iteration or
wall-clock budget — the message distinguishes the four so the remedy is not blind
trial-and-error.

> Source of truth: [`bitblast_oom_skip_note`](../crates/mununu-core/src/planner/mod.rs) — surface: (CLI+API+UI)

### Compound-atom safety escalation + `bottom-reason` diagnostic (mununu#492)

> Source of truth: [`reduce_ag_boolean_body`](../crates/mununu-core/src/adapter/reach_rescue.rs) + [`emit_ag_boolean_invariant_monitor`](../crates/mununu-core/src/adapter/btor2/bad_monitor.rs) — surface: (CLI+API+UI)

**Compound-safety rescue.** A residual ⊥ on a safety property of the shape `AG(compound)` — where `compound` is a boolean tree over `And`/`Or`/`Not`/`True`/`False`/`Predicate` nodes and each `Predicate` leaf is a single `REG op VALUE` comparison — is now decided by the reachability portfolio. Widens the existing single-atom escalation to accept exclusion (`!a || !b`), implication (`!a || b`), and conjunctive (`a && b`) shapes, and — critically — resolves each leaf via state cell → output net → **primary input**, so a zero-state model (a purely combinational block like `mem_router`) can have its properties decided at k=0 without a state cone to walk. Preserves byte-for-byte behavior on the shipped single-atom path (single-atom shapes route through the original emitter; the compound emitter is a fallback).


**Non-overlapped implication (`|=>`) rescue (mununu#503).**

> Source of truth: [`reach_rescue::reduce_ag_implies_next`](../crates/mununu-core/src/adapter/reach_rescue.rs) — surface: (CLI+API+UI, via `sv verify-auto`'s escalation pass)

A residual ⊥ on `A |=> C` — which lifts to `nu X. ((¬A ∨ [] C) ∧ [] X)`, i.e. `AG (A → AX C)` — is now reduced to a `bad` monitor and decided by the reachability portfolio, alongside the `AG(compound)` shape above. The monitor synthesises a 1-bit latch (`mununu_ante_prev`, `init 0`, `next = A`) and asserts `bad = mununu_ante_prev ∧ ¬C`.

Two properties of this path are worth knowing:

- **The antecedent may be compound and the consequent may be combinational.** Both were hard limits of the earlier single-atom emitter, and both are what real assertions need — OpenTitan's `sysrst_ctrl_detect` states its FSM transitions as `state_q == DetectSt && !trigger_active && cfg_enable_i |=> state_q == IdleSt`.
- **It decides properties the concrete oracle cannot cross-check.** `concrete_oracle::ag_implies_next` refuses an input-dependent consequent because it evaluates the consequent as a function of the next *state* and has no next-cycle input to evaluate against. A `bad` monitor has no such gap — the next cycle's inputs are part of the trace. Where a differential is wanted for these, the exact-symbolic engine is the cross-check, not the oracle.

**What still declines.** A register-vs-register leaf (`cnt_q >= cfg_debounce_timer_i`) is a `CmpReg`, which the leaf compiler does not accept; a nested-box consequent (`a |=> ##1 b`, and the nested form the translator emits for multi-element antecedents) is not a boolean body. Both decline as `safety-shape-not-reducible` rather than being mis-compiled — a modality silently flattened into a boolean would be a soundness bug, not a wider reach.

**`bottom-reason` note.** For any property that stays ⊥ after the escalation pass, a new `bottom-reason` verification note classifies WHY the ⊥ stands:

- **`safety-shape-not-reducible`** — the property is Safety-class but **no** reducer accepts its shape (a compound with a register-vs-register `CmpReg` leaf, a nested-box consequent from `##k` / a multi-element antecedent, a non-standard AG outer shape, etc.). NOT the same as a resource abstain — this needs a property reshape or a new rescue reducer, not a bigger budget.
- **`no-state-model-non-safety`** — the design has zero state registers AND this property is non-Safety (liveness / recoverability). On a stateless model, `AG EF good` is vacuously true and `AG(a → AF b)`'s l2s reduction has no lasso to close. The ⊥ is a modelling issue (compose with a stateful context or restrict to tier-1 safety), not an engine cap.
- **`array-atom-unsupported`** (mununu#518) — the property names a **memory** (a BTOR2 array state), not a scalar register. Arrays are encoded as SMT `Array` constants resolved through a separate name map, not as entries in the bit-vector signal vector, so there is no BV for a cube dimension to range over. The note's `items` carries the memory's name. Rephrase over a scalar register (a read port's output, say). Before #518 such an atom was silently admitted as a BV state cell and could produce a confident wrong answer instead of a diagnosable one.
- **`unclassified`** — the classifier did not narrow further; most commonly a resource abstain the engine already flagged with a `bit-cap` / `abstained on the …` sibling note.

Consumers keying on `verification_notes[i].kind` can distinguish "the engine gave up" from "the property shape doesn't fit this rescue lane," and act accordingly. Prior to mununu#492, the two cases were indistinguishable — a critical distinction for contrast pairs where a stateless block's supposed-to-fail twin was returning ⊥ instead of VIOLATED (the ticket's `mem_router_faulty` case).

### Process-wide memory ceiling: `MUNUNU_MAX_PROCESS_MEMORY_BYTES` (mununu#490, default revised in #504)

> Source of truth: [`adapter::memory_budget::check_process_memory_budget`](../crates/mununu-core/src/adapter/memory_budget.rs) — surface: (CLI+API+UI, env var — process-global)

The four exact-engine budgets above (`BIT_CAP` / `NODE` / `ITERATION` / `WALL-CLOCK`) protect the exact engine's own fixpoint. They do NOT stop the default Rust allocator from calling `abort()` (exit 134) on a failed allocation, which crashes the whole process and takes every property in the same invocation down with it. A verify-lane consumer then sees a crash instead of an `unknown` verdict, so a run that would have decided N-1 properties reports none.

`MUNUNU_MAX_PROCESS_MEMORY_BYTES` is a **process-RSS ceiling** — auto-derived from a detected container limit, or set explicitly — that mununu polls itself between properties and at each `escalate_bottom` step. When the ceiling is exceeded, the current + remaining properties abstain (`unknown`) with a `memory-budget-exceeded` verification note, and prior verdicts are preserved. This trades an OS-level `abort()` for a graceful degradation, so a downstream gate can distinguish "ran out of memory" from "the engine did not decide."

**The default changed in mununu#504 C6 — unset now means AUTO, not disabled.**

| value | ceiling |
|---|---|
| a positive integer | that many bytes (explicit; unchanged) |
| `0` | **disabled** — the deliberate escape hatch |
| non-numeric | disabled, with a debug log |
| **unset** | **auto**: 80% of a detected cgroup limit (v2 `/sys/fs/cgroup/memory.max`, then v1 `memory/memory.limit_in_bytes`), or **disabled** when no limit is detected |

```bash
# Explicit — cap at 24 GiB:
MUNUNU_MAX_PROCESS_MEMORY_BYTES=25769803776 mununu sv verify-auto design.sv

# Opt OUT of the auto ceiling entirely:
MUNUNU_MAX_PROCESS_MEMORY_BYTES=0 mununu sv verify-auto design.sv
```

**Why the default moved.** The ceiling converts an allocator `abort()` — which kills every property in the invocation — into per-property abstentions. Defaulting it off meant the protection was absent exactly where it is needed most: a containerised CI lane, whose operator has no reason to know the variable exists until a run has already crashed.

**What it costs, stated plainly.** An auto ceiling can abstain on a run that would have finished, because RSS at 80% of the limit does not guarantee an OOM. Three things bound that: the ceiling engages only when a container limit is actually **detected** (an unconstrained developer machine gets no ceiling and behaves exactly as before), 80% is deliberately loose, and `=0` turns it off. An unlimited cgroup (`max`, or a v1 "unlimited" sentinel) counts as no limit.

**Sizing an explicit value:** 70-80% of the process's real memory limit (`ulimit -m`, container `--memory`), leaving headroom for allocator overhead + non-mununu memory. Sizing too tight causes spurious abstentions on properties that would have decided; sizing too loose lets the process abort before the ceiling fires.

**Coarse granularity.** The check fires BETWEEN checkpoints (per property + per escalation step). It does NOT catch an allocation that fails within a single BDD blast between checkpoints — a single property's blowup can still crash the process. The ceiling is a graceful-degradation lever for the multi-property `sv verify-auto` lane, not an absolute crash guarantee.

**Complementary to `--config-value`.** The bit cap counts kept cone bits AFTER COI and AFTER pinning, so `--config-value SIG=V` shrinks the real problem and often keeps a property under the memory ceiling that would otherwise trip it. `@mununu_predicate` hints do NOT — they seed cube dimensions, and an alternating νμ formula is decided by the exact engine which does not use them.

### Crash-survivable partial verdicts: `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` (mununu#504 C7)

> Source of truth: [`adapter::partial_json::Breadcrumb`](../crates/mununu-core/src/adapter/partial_json.rs) — surface: (CLI+API+UI, env var — process-global)

It is read inside `verify_auto` itself, so it applies to **any** process running a verification — the CLI, and equally the API server, which can be OOM-killed mid-request and would otherwise return nothing for properties it had already decided.

The memory ceiling above degrades gracefully when mununu can observe its own trouble. It cannot help when the process is killed from **outside** — the kernel OOM killer, a CI step timeout, `docker stop`. `SIGKILL` runs no Rust code: no destructor, no panic handler, no report. A lane that verified 24 of 25 properties then reports nothing at all.

Point `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` at a path and `sv verify-auto` appends one JSON object per line as each property completes, flushing after every write:

```bash
MUNUNU_VERIFY_AUTO_PARTIAL_JSON=/tmp/verdicts.ndjson mununu sv verify-auto design.sv
```

```json
{"index":0,"property":"fifo_sva_0","outcome":"holds","phase":"main"}
{"index":1,"property":"fifo_sva_1","outcome":"unknown","phase":"main"}
{"index":1,"property":"fifo_sva_1","outcome":"holds","phase":"escalated"}
```

**Read the LAST record per property name.** A verdict can change after the main loop — the ⊥ re-plan pass may turn an `unknown` into a definite verdict, and appends a second record with `"phase":"escalated"`. Only properties that actually moved get a second record.

**It is a diagnostic, never a gate.** An unwritable path, a full disk, or a serialization problem is swallowed after a single warning; a breadcrumb must not be able to fail a verification run, which would trade a recoverable crash for an unconditional one.

**One path, one run.** The file is truncated on open, so a process that calls `verify_auto` more than once — the API server across requests, `sv mutate`'s baseline-then-mutant pair — reuses the path and the last opener wins. Use a distinct path per run when that matters.

**Not a substitute for the report.** It carries verdicts only — no counterexamples, notes, or seeded predicates. A run that finishes normally still emits the full report, and consumers should prefer it. This is for the run that does not get to finish.

### Pinning a config input: `--config-value`

> Source of truth: [`partition_config_pins`](../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: (CLI+API+UI)

`--config-value SIGNAL=VALUE` (API `config_values: [...]`) pins a primary input to
a decimal constant and substitutes it into every formula atom, scoping the verdicts
to that value (a `config-concretization` note lists the applied pins). A pin that
mununu **cannot apply** is a hard **error**, never a silent drop that would verify a
different question than the one asked: a non-decimal value (`rst_n=0x1` — decimal
`u64` only) is rejected at parse; a `SIGNAL` that is not a primary input of the
lifted model (misspelled, or a state/output/optimized-away name) is rejected against
the model's real inputs. Every applied pin — including one that coincides with an
auto-detected reset pin — is echoed in the `config-concretization` note.

### SVA `|=>` with input-derived antecedents: automatic shadow-synth

> Source of truth: [`antecedent_shadow`](../crates/mununu-core/src/adapter/btor2/antecedent_shadow.rs) + [`detect_pipeimplies_antecedent_atoms`](../crates/mununu-core/src/mu_calculus/mod.rs) — surface: (CLI+API+UI, engine-internal, no user flag required)

SVA `A |=> C` whose antecedent `A` reads primary inputs — directly OR through a
combinational chain (`mem_rvalid_mine = mem_rvalid && (mem_rid == CLIENT_ID)`,
`valid_and_ready = valid && ready`, address decoders, enable stacks) — is the
common shape in real RTL. The exact-symbolic engine leaves inputs FREE per
modality step, so pinning an input-derived antecedent decouples the antecedent
copy from the transition copy of the same physical signal. Left alone, the
engine would return a spurious verdict on correct RTL (`Violated (1 cell)` on
monono's `wb_mem_client` was the mununu#476 report).

**Automatic fix — antecedent shadow-register synthesis.** At verify time the
engine detects the canonical `|=>` lift shape `nu X. ((¬A ∨ □B) ∧ □X)` in the
mu-calc formula and, for every antecedent `A` whose combinational cone reaches
primary inputs, synthesises a **shadow state cell** `_mununu_antshadow_<N>` in
the BTOR2:

- `init = 0` — SVA `|=>` semantics say cycle 0 has no prior antecedent, so the
  obligation is trivially satisfied at reset.
- `next = A` — the shadow samples `A` each cycle, so at cycle N+1 it carries
  `A@N`.

The antecedent atom is rewritten to reference the shadow; the exact engine
evaluates on the augmented model. Verdicts that were previously `Skipped`
(under the earlier transitive refusal) now decide correctly. Standard
SVA-to-BMC compilation technique (SymbiYosys / JasperGold / EBMC do the
equivalent). Full design + soundness argument:
[`docs/design/antecedent-shadow-synthesis.md`](design/antecedent-shadow-synthesis.md).

**Fallback conditions — a refusal, not an unsound verdict.** Five cases fall
through to the earlier Phase A refusal (definite `Skipped`, never a wrong
answer):

- Non-Boolean antecedent (`|=>` should be Boolean; wider is a language misuse).
- Array/memory in the antecedent's cone (havoc defeats the shadow).
- The antecedent atom is **itself** a primary input (rare, author-confirmation
  case — restate as `AG(prev(A) → C)` if truly desired).
- Cone reaches an anonymous free input from a partial-write havoc
  (a different soundness posture; see `signal_reaches_anonymous_input`).
- Any leaf inside a multi-atom antecedent that itself hits one of the four
  conditions above (e.g. a leaf that IS a primary input). The remaining
  leaves still get shadows; only the problematic leaf falls through to the
  Phase A refusal.

Multi-atom antecedents like `(a && b) |=> c`, `(a || b) |=> c`, and
`(a && !b) |=> c` are supported as of 2026-08 (extended from the initial
single-atom scope). The detector walks any Boolean subtree under the
antecedent-side `Not` and returns every `Predicate` leaf as an independent
shadow target. Independent shadows compose correctly:
`shadow(A) ∧ shadow(B) = A@N ∧ B@N`, `shadow(A) ∨ shadow(B) = A@N ∨ B@N`,
`!shadow(A) = !A@N`.

**Opt-out** — three channels, thread-safe per-request first, then process-global:

1. **CLI flag** `--no-antecedent-shadow` on `sv verify-auto` (mununu#476 item 4,
   added 2026-08). Per-invocation, thread-safe.
2. **API field** `no_antecedent_shadow: true` on `POST /api/v1/sv/verify-auto`
   (mununu#476 item 4). Per-request, thread-safe — the correct opt-out for a
   multi-tenant server.
3. **Env var** `MUNUNU_NO_ANTECEDENT_SHADOW=1`. Process-global; races across
   concurrent verify-auto calls with different intents. Kept as an
   ergonomic escape hatch for scripts and CI shells that never spawn
   concurrent verify-auto calls.

Either channel disabling shadow-synth wins. All three are debug /
differential-oracle knobs (used to cross-check shadow-synth verdicts against
the predicate-cube engine's independent handling); production use should
leave all three unset.

**Non-`|=>` formulas are unaffected.** A hand-authored `mu Y. (a or <> Y)`
(bare `EF a`) whose atom happens to be input-derived does NOT match the
detector and still hits the refusal — the shadow rewrite is only sound for the
`|=>` shape. Direct `btor2 verify` callers with such formulas are unchanged.

---

## Driving it from an external agent (e.g. an agent writing RTL)

> Concept: integrating mununu's HTTP API into an automated design/verify loop.

An external tool — including an LLM agent generating RTL — can `POST` a generated
module to `/api/v1/sv/verify-auto` and get back per-property verdicts to gate or
repair its output. Start the server with:

```bash
mununu server --addr 127.0.0.1:8080   # built with --features api
```

Three caveats are load-bearing; plan for them.

### 1. The server host needs the SV toolchain

`sv verify-auto` shells out to **slang** (elaboration + SVA), **sv2v**
(normalisation), and **Yosys** (SV → BTOR2). These are **not bundled** — mununu
discovers each via a `locate_*` helper (a `MUNUNU_<TOOL>_PATH` env var, then `$PATH`)
and returns a **structured error** if one is missing, rather than crashing. Run the
API on a host that has them installed (the pinned `docker/Dockerfile.sva` image is the
supported environment). See [`external-tools.md`](external-tools.md).

**Net-type normalization (lift widening).** A net-type qualifier on a *port* header
(`input tri0 baud8x`) is rejected by both yosys's reader (a parse error) and slang's
lowering (`net type 'tri0' unsupported`) — the pull / wired-resolution of
`tri0`/`tri1`/`wand`/`wor`/… is a strength-resolution (simulation) concept a word-level
backend does not model. mununu strips such a qualifier on **port** declarations before
staging the source, so the design parses: a port net type only governs that port's
*external* resolution, so for isolated single-module verification an input is havoc'd and
an output is driven internally — dropping it is **sound** (exact for inputs). **Internal**
net-type declarations are left untouched (their pull / resolution semantics matter).

> Source of truth: [`strip_port_net_types`](../crates/mununu-core/src/adapter/yosys/mod.rs#L254) — surface: (CLI+API+UI)

**Wider front-end via yosys-slang (opt-in).** yosys's native `read_verilog` rejects some legal
SystemVerilog a full front-end accepts — e.g. a bounded `while` loop in an `always @*` block —
and black-boxes the module (all its properties then skip). Set `MUNUNU_YOSYS_FRONTEND=slang` (when
the yosys-slang `read_slang` plugin is present — `MUNUNU_YOSYS_SLANG_PLUGIN`, or the pinned image's
`share/yosys/plugins/slang.so`) to lift the RTL with `read_slang` (a complete SV front-end) instead.
It reads the RTL **only** (`--ignore-assertions` — SVA is extracted separately, so nothing is lost)
and names named registers/ports **identically** to `read_verilog`, so cutpoints / `--config-value` /
atoms resolve unchanged. *(Example: the `xgate` coprocessor, whose RISC core has a rejected `while`
loop, is black-boxed under `read_verilog` but lifts its full 125-register core under `read_slang`.)*

The default `--frontend auto` tries `read_verilog` (+sv2v) first and **falls back to `read_slang`**
on a lift error (`MUNUNU_YOSYS_FRONTEND=slang` flips the preference). The two paths are **not
interchangeable for soundness** — `read_slang` and `read_verilog` model a *partial* register write
(`q[hi:lo] <= d` / `q[idx] <= d`) differently (see the partial-write refusal / cone fix), so a silent
choice between them can be green about a different question than the one asked. Verify-auto therefore
**reports which front end produced the verdicts** as a `lift-frontend` note — Info when there was no
choice, and a **ScopeCaveat naming the fallback reason** when `auto` fell back. Note also that SVA in
a `bind`-ed file cannot be parsed by `read_verilog`, so `auto` **always** ends on slang there.

> Source of truth: [`slang_frontend_selection`](../crates/mununu-core/src/adapter/yosys/mod.rs#L1672) — surface: (CLI+API+UI)
> Source of truth: [`SvFrontend::lift_label`](../crates/mununu-core/src/adapter/yosys/mod.rs) + the `lift-frontend` note in [`build_notes`](../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: (CLI+API+UI)

### 2. Properties come from the module's own assertions

`sv verify-auto` checks the SVA the design *carries*; it takes no formula. An agent
that emits RTL **with** `assert property` statements gets those checked automatically.
An agent that emits assertion-free RTL gets zero properties back — it names the
property it cares about with an **SV-direct verb**, which lifts the module *and*
decides the property in one call:

> Source of truth: [`sv_verify::sv_verify_recoverability`](../crates/mununu-core/src/adapter/sv_verify.rs#L84) — surface: (CLI+API+UI)

```bash
mununu sv verify-recoverability my_module.sv --target "state_q == 3"
mununu sv verify-liveness      my_module.sv --request "req == 1" --grant "grant == 1"
mununu sv verify               my_module.sv                       # safety of its assertions
```

Same over HTTP: `POST /api/v1/sv/verify` · `/sv/verify-liveness` ·
`/sv/verify-recoverability`. These let the agent name *any* property — including the
branching ones (recoverability) SVA cannot state at all — against raw SV, no BTOR2
round-trip. (The two-step `sv emit-btor2-per-module` → `btor2 verify-*` path still
works when you want the intermediate BTOR2.)

### 3. Coverage is a fragment, and verdicts are honest

The SVA translator supports a defined fragment (implication `|->` / `|=>` over
**bounded sequences** on either side, `$past` (including a depth `$past(x, k)`,
k ≥ 1) / `$stable` / `$rose` / `$fell`, `$onehot` / `$onehot0`, …); assertions
outside it come back in the `unsupported` list. The bounded-sequence layer covers
cycle delays (`##k`, `##[m:n]`), fixed consecutive repetition (`b[*n]`), and
multi-element `##` chains of booleans (`a |-> b ##1 c ##2 d`) — in the consequent
*and*, at fixed delays, the antecedent (`(a ##1 b) |-> c`, `a[*n] |-> c`). Each
lowers to nested `[]` — `a |-> ##2 b` → `AG(a → AX² b)`; `(a ##1 b) |-> c` →
`AG(a → AX(b → c))` — so a definite verdict transfers just like the plain `|=>`
(one `[]`) case. What stays out of fragment (rejected with a reason, never
dropped — it needs a SERE→automaton subsystem): **unbounded** `##[m:$]` / `[*n:$]`,
**goto** `[->n]` / **non-consecutive** `[=n]` repetition, range delays/repetition
*inside* a multi-element chain or in an antecedent, and repetition/nesting inside
a chain element.

The history functions work over a **register or a primary input**. That second
half matters more than it sounds: it is what makes a data-integrity property —
"what goes in comes out" — checkable at all, since the antecedent samples an
input.

> Source of truth: [`augment_with_past_shadows`](../crates/mununu-core/src/adapter/btor2/shadow.rs) — surface: (CLI+API+UI)

```systemverilog
(push && !pop && cnt_q == 0) |=> (d0_q == $past(din))
```

Each `$past` base becomes a real flop in the model (`next(b__past) = b`), so the
verdict transfers to the concrete design. `$past(x, k)` becomes a **k-stage shift
chain** (`b__past ← b`, `b__past2 ← b__past`, … `b__past{k}`); the common
`$past(x)` is the 1-stage case. Each stage is a **history variable** — its next
value is a function of the present, nothing in the design reads it, so the
augmented system is a conservative extension of the original: it can neither
create nor destroy a counterexample (the argument composes stage by stage). The
base may be a register or a primary **input**; both produce the same chain.

Only the initial cycles need care, because a history variable has no value before
the first transition. A state cell's stages mirror its `init`, so before the chain
fills (cycles `< j` for stage `j`) `$past(b, j)` reads `b`'s reset value. An input
has no `init` to mirror, so **every stage is pinned to an explicit zero** rather
than left free — an emitted `init` line, not an implicit convention, which is why
every engine reads the same value for it.

That explicitness became load-bearing in 2026-09 (mununu#498 sibling). An
init-*less* BTOR2 cell used to read as 0 to the cube and exact engines while
staying free for the reachability portfolio, and that split was a verdict
disagreement rather than a nuance. The exact engine now follows the BTOR2 format —
no `init` line means the initial value is unconstrained — so it agrees with the
portfolio, and only the cube path still applies the zero default. A `$past` stage
is unaffected either way precisely because its `init` is written down.

That invented value is read only in the first few cycles after reset. The lift
settles it structurally: `|=>` places the atom under a `[]`, so it is evaluated
only at states that have a predecessor — for depth 1 that fully hides the invented
value, while `|->` evaluates it at the initial state too. So a depth-1
data-integrity property (`|=>` by construction) is unaffected; a `|->` property
over `$past` of an input reads an invented cycle-0 history and its verdict there
does not transfer. For a **depth-k** `$past` of an input the chain takes k cycles
to fill, so even under a `[]` the first k-1 post-reset cycles read the invented
zero — an over-approximation bounded to those cycles (a register base reads its
reset value there, which is the SVA convention, not an approximation).

Two caveats that used to be about `$past` under must-edge inference. Both are now
fixed at the lift (2026-08-28): the abstraction enforces the KMTS invariant
`R_must ⊆ R_may` where must-edges are created — it no longer fabricates a must-edge
out of an unsatisfiable predicate cube, and a per-lift check rejects any residual
containment violation before it reaches an engine. So `must_edge_inference` no
longer reports a spurious `violated` for a correct `$past` design, and
`--engine symbolic` no longer aborts on one. (Default is still must-edge inference
off, which was already sound for `holds`.)
[`docs/design/past-shadow-soundness.md`](design/past-shadow-soundness.md) carries
the full argument, the per-engine ledger, the §6 fix note, and why no engine may
ever assume stutter equivalence here.

Some designs hit the abstraction ceiling and return
`unknown`. Crucially, **a `holds` or `violated` verdict is sound** (Bruns–Godefroid
3-valued preservation for the abstraction; k-induction / IC3 / BDD for the exact and
portfolio engines) — an agent can trust a definite verdict and treat `unknown` /
`skipped` as "not decided here," not as "safe."

---

## Preflight: `sv lint` — structural faults refused at write time

> Source of truth: [`sv_lint_registers`](../crates/mununu-core/src/adapter/sv_verify.rs#L228) — surface: (CLI+API+UI)

`sv lint` runs a set of **purely structural** checks over the lifted netlist — no
bit-blast, no properties, no environment, ~lift cost. Each finding carries a
`rule` tag:

| `rule` | Catches |
|---|---|
| `undriven-partial-write` | a register whose partial-write lift reaches a free input, so a state predicate over it would be *refused* by the verifier |
| `registered-array-read-moving-address` | a registered array read whose address register can change in the same cycle its data is consumed ([below](#registered-array-reads-against-a-moving-address)) |

Some SystemVerilog is **not lifted faithfully**, and the verifier is honest about
it: a plain-vector partial register assignment (`q[hi:lo] <= d`) leaves `q`'s other
bits undriven, so the yosys-slang front end models them as **free inputs** (havoc)
and aliases the register name to the `concat` that mixes them. A state predicate
over `q` would then read those free inputs, so the engine **refuses** (skips) such a
property rather than emit an unsound verdict (the monono#partsel soundness guard —
see [`recoverability-vs-sva.md`](design/recoverability-vs-sva.md)). The packed-2-D
`q[idx] <= d` split (anonymous *sub-registers*, no free inputs) is kept and decides;
a fully-written register is faithful.

`sv lint` surfaces exactly those refusable registers **at CI time** — it lifts the
design and scans it (~lift cost, **no** model checking, cap-immune, ~0.1 s) so an
unfaithful lift is caught *before* the minutes-long formal gate runs. It **changes
no verdict**; it is a read-only preflight.

```bash
mununu --quiet sv lint rtl/mod.sv --frontend slang        # exit 2 ⇒ a register can't be kept
```

```jsonc
// POST /api/v1/sv/lint  →
{ "signals_flagged": 2, "registers_flagged": 1,
  "findings": [ { "signal": "a_q", "kind": "register", "rule": "undriven-partial-write" },
                { "signal": "o_partsel", "kind": "output", "rule": "undriven-partial-write" } ] }
```

`kind` is `"register"` (the root — the register whose bits are undriven) or
`"output"` (a combinational output that reads one; a property over it is refused for
the same reason). A finding maps to the shared CI gate's `violated` verdict, so the
default `--fail-on violated` fails the build; `--fail-on none` makes it advisory. A
clean design reports zero findings and exits `0`.

### Registered array reads against a moving address

> Source of truth: [`lint_registered_array_read_moving_address`](../crates/mununu-core/src/adapter/sv_verify.rs) — surface: (CLI+API+UI)

```systemverilog
always_ff @(posedge clk) begin
    if (advance) a_q <= a_q + 1;   // the address register moves
    q <= mem[a_q];                 // registered read against it
end
```

`q` holds the word at the address `a_q` held **last** cycle, but a consumer that
reads `q` alongside the live `a_q` sees a pair that never coexisted. Nothing in
the design records the correspondence, so the mismatch is silent and survives
every property that compares the design to itself.

This is not hypothetical: monono hit it **twice in the same block**, the second
time with a prose rule against it in force, shipping a 2 KB sprite bank shifted
by one halfword. Eleven properties, a contrast twin, a CTXDSL model and a frame
CRC all passed; what found it was rebuilding the memory image from the fetcher's
own returns and diffing against the source. Prose did not prevent the
recurrence, which is why the rule is now structural and refused at write time
(mununu#496).

**The satisfying form** registers the address alongside the data, so some
register captures `a_q` in the very cycle the read is issued:

```systemverilog
always_ff @(posedge clk) begin
    if (advance) a_q <= a_q + 1;
    q   <= mem[a_q];
    a_d <= a_q;        // <-- records which address `q` corresponds to
end
```

Like the other lift-form checks, the rule is **satisfiable by writing the
supported form** — here, by naming the tracking signal. Two shapes are never
flagged: an address that is not a register (no moving register to mis-pair
with), and a register held constant (`a_q <= a_q`, or no `next` at all — it can
never disagree with `q`).

**Aliases are resolved** (mununu#506). A register's identity rarely survives on
its raw `state` line: `async2sync` lifts an async reset into a mux, so what the
read actually indexes is `ite(rst_n, state, RESET)`, and the register's *name*
lands on a `uext` alias of that mux. The rule resolves the address — and the
tracking register, and the reported name — back through the identity-preserving
wrappers (`uext` / `sext` / `slice` / a reset-shaped `ite`). Before that it
required a bare `state`, which meant it fired **only on registers with no
reset** — close to none in real RTL, and the reason it reported nothing on the
design that motivated it.

**Arithmetic addresses are in scope** (mununu#506 Tier 1/2). The rule analyses
the address's **cone** rather than demanding a bare register: the address *moves*
when any register in its cone moves, which covers `mem[base + 1]`,
`mem[{tag, a_q}]`, `mem[a_q[hi:lo]]`, `mem[a_q ^ mask]` — the shape class, not a
list of cases. An address driven **only by inputs** stays out of scope: that is
how every block-RAM wrapper is written (`sprite_bank`'s `addr` port), and
maintaining the correspondence there is the caller's obligation, not the
module's. Inputs *alongside* a moving register still flag.

The satisfying form generalises in step — a register capturing the whole address
**expression** (`a_d <= a_q + 1`) counts as tracking it. That symmetry is
load-bearing: widening what counts as a moving address without widening what
counts as recording it would fire on correct RTL. When the two expressions are
semantically equal but structurally different (CSE did not merge them), an SMT
check confirms it. **The solver runs only on the reporting path and can only
withdraw a finding, never create one** — so a clean design never pays for it, and
an encode failure or a solver `Unknown` leaves the structural verdict standing.

Because the lift **flattens**, this also covers the cross-module form — the array
and destination register in a child, the address register in the parent — which
no per-module reading of the RTL would catch.

```jsonc
// POST /api/v1/sv/lint  →
{ "signals_flagged": 1, "registers_flagged": 1,
  "findings": [ { "signal": "q", "kind": "register",
                  "rule": "registered-array-read-moving-address",
                  "detail": "`q` is a registered array read addressed by `a_q`, which can \
                             change in the same cycle `q` is consumed, and no register \
                             captures `a_q` alongside it ..." } ] }
```

**Exit codes** (2026-08, mununu#475 item 5). `sv lint --fail-on violated` (the
default) exits `0` on a clean design, `2` when a bad register is found — a
gate-friendly default that plugs directly into CI. A LIFT FAILURE (front-end
error, missing file, etc.) exits `1` — distinct from "found something". A batch
scanner that wants to distinguish "found a bad register" from "could not lift"
should pass `--fail-on none` and inspect the JSON/text output for findings.

**Flattened hierarchy is no longer suppressed** (2026-09, mununu#506/#507). The
function-argument filter below skipped *every* dotted Op symbol — but on a
flattened integrator every hierarchical alias is dotted (`u_ld_bank.addr`), so
the filter suppressed the whole design. That is why `sv lint` could report zero
findings on an integrator while `verify-auto` refused a property on the same
file: they were not disagreeing, the lint was not looking. The filter now skips a
dotted symbol only when **no prefix of it names an instance scope**, and the
discriminator is self-contained — *a function has no registers*, so a prefix that
appears on a `state` symbol names an instance. Residual: an instance containing no
state at all is still skipped.

**`function automatic` arguments are no longer false-positives** (2026-08,
mununu#475 item 1). Yosys/slang mangle SV function arguments as
`<function>.<arg>` (e.g. monono's `ctrl_code.c`, `ones8.v` in
`tmds_encoder.sv`). Their combinational cone reaches a function-scope anonymous
input, so the raw filter used to flag them as partial-write registers.
`sv lint` now skips any Op-node symbol containing a `.` — a targeted filter
that leaves the intended catches (register aliases like `a_q` from
`q[hi:lo] <= d`, hierarchical State names, etc.) reported as before.

**`--exclude <NAME>` on `--design-dir`** (2026-08, mununu#475 item 2). The
hardcoded skip set (`mutations` / `buggy` / `buggy_artifacts` / `tb` /
`testbench` / `sim` / `figures`) is now extensible per-invocation. Match is
case-insensitive against a single path component — `--exclude faulty` skips
every `.../faulty/...` subtree but leaves `.../faulty_variant/...` alone.
Repeatable. Not a full glob syntax (a richer form is a future extension when a
case demands it). The reporter's whole-tree workflow —
`mununu sv lint --design-dir <root> --exclude faulty` — is now reachable
without adding to the hardcoded list.

**`--search-path <DIR>` for single-file cross-directory submodules** (2026-08,
mununu#475 item 3). A single-file input that instantiates a submodule from a
sibling directory (`mem_sched.sv` → `slot_arbiter` from a peer dir — 53/109
unliftable files in monono's tree) previously errored with `unknown module`.
`--search-path <DIR>` recursively discovers every `.v` / `.sv` under DIR and
stages them alongside the primary input, so cross-directory instantiations
resolve without hand-listing every `--source`. Deduplicates against the
primary file and any explicit `--source` entries by canonical path, and
suppresses short-name collisions (a search-path file with the same filename
as the primary or a staged source is silently skipped rather than emit two
modules of the same name to yosys). The same `--exclude` list filters the
scan. Repeatable. Composable with `--design-dir` — a primary block plus
sibling libraries — for a design + peer-utility layout.

## Are the properties adequate? `sv mutate`

> Source of truth: [`mutate_and_compare`](../crates/mununu-core/src/adapter/slang/verify_auto.rs#L1824) — surface: (CLI+API+UI)

A `holds` verdict is only as strong as the property that produced it — a **vacuous**
property `holds` no matter what the design does. `sv mutate` measures that: it applies
a **named structural mutation** to the design, re-verifies, and reports whether each
property's verdict **flips**. A flip (`holds` → `violated`) confirms the property
genuinely constrains the mutated behaviour; a property that does **not** flip is the
finding — the spec is *vacuous with respect to that fault*.

This is a statement about the **properties**, never a bug report about the design
(see [`policies/claims-integrity.md`](policies/claims-integrity.md) §2) — a mutation is
a deliberately-injected fault, not a discovered one.

The mutation catalog — two **structural** (no targeting) and two **targeted** (a
named-signal + structural-cone resolution, never a source line, which the sv2v lift
does not preserve):

- **`stick:<reg>`** — freeze a register at its reset value (`next(reg) := reg`).
  Universal; flips any property that depends on the register progressing.
- **`drop-reset:<reg>`** — remove a register's reset arm (rewrite its reset mux
  `ite(rst, RESET, d)` to the data branch `d`). Flips a reset-dependent property;
  needs the reset **free** (`--no-gate-reset`) to have effect.
- **`off-by-one:<reg>[@<const_nid>][:±1]`** — perturb by ±1 the constant a register
  is compared against (`cnt < 8` → `cnt < 9`); the classic boundary fault. The bound
  is found by walking the comparison cone in the register's fanout; when the register
  is compared against more than one constant the error lists the candidates and
  `@<const_nid>` disambiguates. Default delta `+1`.
- **`invert-cond:<sig>`** — invert a named **1-bit** condition at every use site
  (flip the polarity of every operand referencing it, via BTOR2's `-N` bit-not
  shorthand). Flips a property whose truth depends on the guard's polarity.

`sv mutate --list` enumerates all four target classes (`stick` / `drop_reset` /
`off_by_one` / `invert_cond`).

```bash
mununu sv mutate counter.sv --mutation stick:cnt --engine exact-symbolic   # exit 2 ⇒ no property caught it
mununu sv mutate counter.sv --list                                         # discover the targets
```
```jsonc
// POST /api/v1/sv/mutate  { "source": …, "mutation": "stick:cnt", "engine": "exact-symbolic" } →
{ "mutation": "stick:cnt", "targets": null, "flipped": 1, "unflipped": 0,
  "properties": [ { "name": "recoverable", "baseline": "holds", "mutant": "violated", "flipped": true } ] }
```

A mutation caught by **no** property maps to the CI gate's `violated` (a coverage gap),
so default `--fail-on violated` fails the build; `--fail-on none` is advisory. `--list`
always exits `0`. A mutation that names a missing register — or does not apply (e.g.
`drop-reset` on a register with no reset mux) — is a hard **error**, never a silent
no-op (which would masquerade as an unflipped/adequacy finding).

**`--param NAME=VALUE` parity with `sv verify-auto`** (2026-08, mununu#475 item 4).
`sv mutate` now accepts `--param`, plumbed through to the same yosys `chparam` /
slang `-G` path `verify-auto` uses. Blocks whose adequacy measurement had to shrink
a parameterised timing interval (`--param ROW_BITS=2` and similar are load-bearing
in monono's tree) can now be mutated end-to-end without a wrapper module. Same
semantics as `verify-auto`'s `--param`: a malformed value is a HARD error; yosys
errors downstream on a parameter it cannot apply — never a silent drop.

## Using mununu in CI (GitHub Actions and friends)

> Source of truth: [`FailOn` / `ci_exit_code`](../crates/mununu-cli/src/main.rs) — surface: CLI

Every verify verb (and `sv verify-auto`) is a **CI gate**: it maps the verdict to a
process **exit code** so a workflow step fails on a real violation without parsing
JSON.

| Exit code | Meaning |
|-----------|---------|
| `0` | property holds (or all properties hold) — the step passes |
| `2` | a property is **violated** — the step fails |
| `3` | a property is **unknown** *and* `--fail-on unknown` was set |
| `4` | **an expectation was not met** (mununu#537) — the run worked and disagreed with a `--expect-*` claim. Distinct from `1` (the run failed) on purpose, and it SUPERSEDES `--fail-on`: under `--expect-violated`, `2` would fire on the very violation you asked for. |
| `1` | tool / usage error (bad file, unparseable atom, missing toolchain) |

- `--fail-on <violated\|unknown\|none>` picks the gate policy. Default `violated`:
  an undecided `unknown` does **not** fail the build (it is "not decided," not
  "broken"). Use `--fail-on unknown` for a strict gate, or `--fail-on none` to
  report-only (always exit `0`).
- `--quiet` (global) suppresses the `logs/mununu.log` workspace file and the startup
  banner — errors only to stderr, so the workspace stays clean and stdout is the JSON.

```yaml
# .github/workflows/verify.yml — fail the build if the FSM can't recover to idle
jobs:
  recoverability:
    runs-on: ubuntu-latest
    container: ghcr.io/mumunu-team/mununu-sva:latest   # bundles slang + sv2v + yosys
    steps:
      - uses: actions/checkout@v4
      - run: |
          mununu --quiet sv verify-recoverability rtl/fsm.sv \
            --preprocess-sv2v --target "state_q == 0"   # exit 2 ⇒ step fails
```

For a whole module's assertions in one gate, `mununu --quiet sv verify-auto rtl/mod.sv
--preprocess-sv2v --json` exits non-zero iff any property is violated; the JSON on
stdout carries the per-property detail for a summary step. `verify-auto` also
**auto-escalates** a *safety* property the cube abstraction leaves `⊥` to the
multi-engine reachability portfolio (`--no-rescue` opts out), recording a
`portfolio-rescue` note if the portfolio decides it.

The full CI recipe (a complete GitHub Actions workflow) and the agent-over-HTTP
recipe live in
[`wiki/CI-and-Agent-Integration.md`](../wiki/CI-and-Agent-Integration.md).

---

## See also

- [`../wiki/CI-and-Agent-Integration.md`](../wiki/CI-and-Agent-Integration.md) — the
  full CI (GitHub Actions) + agent-over-HTTP integration guide.
- [`design/recoverability-vs-sva.md`](design/recoverability-vs-sva.md) — why `AG EF`
  recoverability is outside SVA, with the OpenTitan `csrng` worked example.
- [`cli-cookbook.md`](cli-cookbook.md) — common `mununu` CLI invocations.
- [`external-tools.md`](external-tools.md) — installing slang / sv2v / Yosys and the
  discovery env vars.
- [`../wiki/Verify-Project-Flow.md`](../wiki/Verify-Project-Flow.md) — the N-source
  `verify.toml` framework for composing multiple sources.
