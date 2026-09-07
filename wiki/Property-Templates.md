# Property Templates

> **Alpha Software** — Mununu is under active development. APIs, syntax, and behavior may change.

Property templates provide parameterized mu-calculus formula patterns that can be instantiated across any domain — SystemVerilog RTL, MCP/agentic protocols, software extraction, and synthesis benchmarks. Templates hide formal logic behind named patterns with human-readable descriptions.

## Overview

Instead of writing raw mu-calculus:

```
nu X. (<> true && [] X)
```

Use a named template:

```bash
mununu context eval player.espec.json --template no_deadlock --automaton PlayerState
```

Templates resolve to standard `PropertyFormula::MuCalculus(String)` at instantiation time. The emitter, evaluator, and synthesis pipeline see no difference from a hand-written formula.

## Built-in Templates

| ID | Display Name | Kind | Parameters | Formula Pattern |
|----|-------------|------|------------|----------------|
| `no_deadlock` | No Deadlock | safety | — | `nu X. (<> true && [] X)` |
| `reachable` | Reachable State | liveness | `$TARGET` | `mu X. (${TARGET} \|\| <> X)` |
| `never` | Never (Invariant) | safety | `$BAD` | `nu X. (!${BAD} && [] X)` |
| `always_eventually` | Always Eventually | liveness | `$TARGET` | `nu Y. ((mu X. (${TARGET} \|\| <> X)) && [] Y)` |
| `bounded` | Bounded Resource | safety | `$OVERFLOW`, `$UNDERFLOW`* | `nu X. (!${OVERFLOW} && !${UNDERFLOW} && [] X)` |
| `response` | Response (Request-Grant) | liveness | `$TRIGGER`, `$RESPONSE` | `nu X. ((!${TRIGGER} \|\| mu Y. (${RESPONSE} \|\| <> Y)) && [] X)` |
| `mutual_exclusion` | Mutual Exclusion | safety | `$A`, `$B` | `nu X. (!(${A} && ${B}) && [] X)` |
| `label_blocked_in_state` | Label Blocked in State | safety | `$STATE`, `$LABEL` | `nu X. ((!${STATE} \|\| [${LABEL}] false) && [] X)` |

\* `$UNDERFLOW` is optional with default `false`.

## Behavioral property patterns by design class

> Source of truth: [`scan_annotation_properties`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/slang/verify_auto.rs#L995) — surface: (CLI+API+UI)

Beyond the atomic templates above, common **behavior classes** warrant recurring *branching-time* patterns —
`AG EF` recoverability, `EF` reachability, error-recovery round-trips — that a plain safety invariant cannot
state. Each pattern below is a parameterized mu-calculus formula you instantiate by substituting the design's
observable signals into the `<slots>` (an atom: `<sig> == <val>`, `<sig> != <val>`, or a bare boolean output),
then verify with `mununu sv verify-auto` via a `@mununu_guarantee` annotation (or `mununu context eval`).
Shorthand → mu-calculus is the [Mu-Calculus Reference](Mu-Calculus-Reference.md):
`EF p = mu X.(p || <> X)`, `AG p = nu S.(p && [] S)`, `AF p = mu Y.(p || [] Y)`,
`AG EF p = nu Y.((mu X.(p || <> X)) && [] Y)`, `AG(a → EF b) = nu Z.((!(a) || mu Y.(b || <> Y)) && [] Z)`.

**Soundness default — use `EF` (reachable), not `AF` (inevitable).** `AF`/box-`AF` is sound only where no free
input (a kick, a clock-stretch, a competing request, a bus-cycle drop) can stall the antecedent path forever.
On the `sv verify-auto` flagship path, a `// @mununu_assume GF <REG op VALUE>` assumption is **auto-applied
to any response-shape guarantee** (`AG(a → AF b)`) via the Emerson–Lei fair-cycle l2s (mununu#477 Option B):
each `GF` assume adds a `fair_seen` monitor latch, and a `holds` verdict under fairness means the guarantee
holds against every environment schedule satisfying the assumption infinitely often. For assumptions that
don't fit `GF <REG op VALUE>` — a state predicate, a non-response guarantee, or coupled multi-guarantee
fairness — use `mununu btor2 verify-liveness-under-fairness` on the emitted BTOR2, or model in CTXDSL where
the GR(1) game engine `(⋀ GF envᵢ) → (⋀ GF sysⱼ)` discharges multi-pair assume-guarantee liveness directly.
When unsure and staying on `sv verify-auto` without a matching `@mununu_assume GF`, prefer `EF`.

| class | trigger | patterns (shorthand, `<slots>` = observable signals) |
|---|---|---|
| **FIFO / buffer / queue** | full/empty/level, push/pop | `EF <full>`, `EF <empty>` (extremes reachable); `AG EF !<full>` (always drainable, never wedges); `AG(<full> → EF !<full>)` (stuck-at-full fails); `AG(<wr_full> → EF !<rd_empty>)` (CDC data-visibility); `AG !(<full> && <empty>)` (safety, if exclusive) |
| **timer / watchdog / recurring pulse** | timeout, expire, tick, pps | `AG EF <ev>==1` (can **always fire again** — fire-once-then-wedge fails) + `EF <ev>==1`; `AG(<ev>==1 → EF <ev>==0)` (pulse doesn't stick); `AG EF <ev>==0` (kick always available); `AG(<armed> → AF <ev>==1)` **only** if no input can stall it |
| **protocol w/ error-recovery** | error/AL/NACK/collision/abort | `EF <error>==1` (error reachable); `AG(<error>==1 → EF <idle>)` (**does NOT trap in error** — the sharpest); `AG(<error>==1 → EF <error>==0)` (flag clears) |
| **CPU / sequencer core** | fetch/decode/execute, stall | `AG EF <fetch>` (no hang); `AG(<stall>==1 → EF <stall>==0)` (stall resolves); `AG(<exec>==1 → EF <fetch>)` (retire) |
| **bridge / arbiter / shared resource** | two interfaces, req/grant | `AG(<req_in>==1 → EF <resp_out>==1)` (forward); `AG(<b_event>==1 → EF <a_out>==1)` (return); `AG !(<g_a>==1 && <g_b>==1)` (mutex); `AG(<req>==1 → AF <grant>==1)` (fair, AF only if no starver) |
| **mode / phase machine** | modes, phases, speed settings | `EF <mode>==k` for **every** documented mode (a dead mode = dead feature); `AG(<mode>==A → EF <mode>==B)` (transition); `AG(<phase>==late → EF <phase>==early)` (restart) |
| **resource release** | shared/open-drain bus, lock | `AG EF <released>` (never permanently locks the bus) + `EF <held>` |

Example — an I²C master (protocol + shared-bus classes), `<error>` = `AL == 1`, `<idle>` = `BUSY == 0`,
`<released>` = `scl_padoen_o == 1`:

```
// @mununu_guarantee nu Z.((!(AL == 1) || mu Y.(BUSY == 0 || <> Y)) && [] Z)   // does not trap in arbitration-loss
// @mununu_guarantee nu Y.((mu X.(scl_padoen_o == 1 || <> X)) && [] Y)          // always releases the bus
```

> **Decidability note.** These patterns are *definite* on compact control FSMs (`--engine exact-symbolic`) and
> decide via predicate abstraction (`--engine explicit --must-edge-inference smt-hyper-must`) when the event
> grounds on a state/registered signal. An event gated by a **deep counter** (a timeout counting to a value)
> may return **⊥** in the cube — that is an honest "not decided", never a pass; see
> [Hardware Verification Patterns](Hardware-Verification-Patterns.md) for the ranking/recoverability path.

## Abstraction-predicate hints (`@mununu_predicate`)

> Source of truth: [`MununuTag::Predicate`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/mununu_annotations/mod.rs) / [`seed_from_formula`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: (CLI+API+UI)

When a cube ⊥ is caused by a *missing* abstraction predicate — the property's own atoms don't pin the
inductive invariant that decides it — you can **seed extra cube dimensions** without changing the property.
Three equivalent surfaces, all merged:

- **In-source annotation:** `// @mununu_predicate <expr>` alongside the design (the property-writing agent's
  in-file channel).
- **CLI:** `mununu sv verify-auto … --predicate "<expr>"` (repeatable).
- **API / UI:** the `predicate: string[]` field / the "Abstraction-predicate hints" box.

`<expr>` is a predicate expression — a literal `reg == value`, a relational `reg == reg`, or a bound
`reg >= K` — routed through the SAME classification as a formula atom (state → cube dimension, state-only
combinational → dimension, input-dependent combinational → derived label, unresolvable → dropped).

```systemverilog
// @mununu_predicate byte_controller.c_state == 0   // the FSM's idle/completion invariant
// @mununu_predicate wptr == rptr                    // a FIFO's "drained" relation
// @mununu_guarantee nu Y.((mu X.(scl_padoen_o == 1 || <> X)) && [] Y)
```

**Soundness.** Predicate abstraction is *monotone*: a hint only refines the may/must partition, so a ⊥ can
become definite but a definite `HOLDS`/`VIOLATED` can **never** flip, and a mis-bound hint is dropped — you
can suggest freely. A hint is *not* an assumption (it never constrains the design). Its value is two-fold:
turning a ⊥ into a decide when the deciding invariant is a statable *state* relation, and **removing a
spurious `VIOLATED`** (a cutpoint over-approximation the extra dimension refutes). It does **not** decide a
recoverability target that is itself a *combinational* output — that needs the target bound as a derived
predicate, not a state hint.

## Using Templates

### CLI

```bash
# Zero-parameter template
mununu context eval model.espec.json --template no_deadlock --automaton FSM

# Template with arguments
mununu context eval model.espec.json \
    --template reachable --template-arg TARGET=GoalState --automaton FSM

# Multiple arguments
mununu context eval model.espec.json \
    --template mutual_exclusion --template-arg A=P1_Active --template-arg B=P2_Active \
    --automaton System

# List all templates
mununu templates

# Filter by domain
mununu templates --domain rtl

# Show template details
mununu templates --id reachable

# JSON output
mununu templates --json
```

`--template` and `--formula` are mutually exclusive. When `--template` is provided, the template is instantiated and injected as an ad-hoc formula.

### In `verify.toml`

> Source of truth: [`PropertySpec`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/verify/config.rs#L307) — surface: CLI+API.

```toml
[[properties]]
name = "goal_reachable"
template = "reachable"
args = { TARGET = "GoalState" }
over = "System"
```

**`TARGET` and other `Predicate`/`State` parameters must be bare identifiers**
(alphanumeric plus `_`) — see
[`validate_param_value`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/templates/mod.rs#L305).
This applies to **both** template surfaces: `verify.toml` `args` and
`context eval --template-arg`. An expression is rejected on either:

```
template instantiation failed: parameter 'TARGET':
  expected identifier (alphanumeric + underscore), got 'n == 2'
```

For an expression target, use the raw `formula` field instead, which takes the
full mu-calculus expression language:

```toml
[[properties]]
name = "counter_reaches_two"
formula = "mu X . ((n == 2) || (<> X))"
over = "System"
```

### In Extraction Specs (`.espec.json`)

Properties can reference templates instead of raw formulas:

```json
{
  "properties": [
    {
      "id": "no_softlock",
      "template_ref": { "template": "no_deadlock" },
      "over": "PlayerState"
    },
    {
      "id": "can_return_idle",
      "template_ref": {
        "template": "always_eventually",
        "args": { "TARGET": "Idle" }
      },
      "over": "PlayerState"
    },
    {
      "id": "custom_formula",
      "formula": "nu X. ([] X)",
      "description": "Raw formula still works"
    }
  ]
}
```

When both `formula` and `template_ref` are present, `formula` takes precedence.

### In SV Sidecars (`.mununu.json`)

```json
{
  "properties": [
    {
      "id": "no_overflow",
      "template_ref": {
        "template": "bounded",
        "args": { "OVERFLOW": "fill_5" }
      }
    }
  ]
}
```

### In XState `__mununu` Block

```json
{
  "__mununu": {
    "properties": [
      {
        "name": "safe",
        "template_ref": { "template": "no_deadlock" },
        "role": "guarantee"
      }
    ]
  }
}
```

### API

```bash
# List templates
curl http://localhost:8080/api/v1/templates

# Filter by domain
curl http://localhost:8080/api/v1/templates?domain=rtl

# Verify with template
curl -X POST http://localhost:8080/api/v1/context/verify \
  -H "Content-Type: application/json" \
  -d '{
    "context": {"name": "model.ctxdsl", "content": "..."},
    "template_ref": {"template": "reachable", "args": {"TARGET": "Idle"}},
    "automaton": "FSM"
  }'
```

### Web UI

In the verification tab, check "Use Template" to switch from formula-name input to the template picker. Select a template, fill in parameters, preview the instantiated formula, and click "Apply Template" to verify.

## Domain Hints

Templates include domain-specific hints that appear in the UI parameter inputs:

| Template | RTL | Agentic | Software |
|----------|-----|---------|----------|
| `reachable($TARGET)` | state_IDLE, fill_0 | SessionClosed | Released, Disposed |
| `never($BAD)` | overflow, error_state | Unauthorized | NullState |
| `bounded($OVERFLOW)` | fill_5 | — | count_max |

## Adding Custom Templates

Templates are defined in `crates/mununu-core/src/adapter/templates/builtin_templates.json`. To add a new template:

1. Add an entry to the `templates` array with `id`, `display_name`, `description`, `kind`, `role`, `domains`, `params`, `formula_pattern`, `domain_hints`, and `tags`.
2. Ensure the `formula_pattern` uses `${PARAM_NAME}` placeholders matching the `params` entries.
3. The template is automatically available in CLI, API, UI, and spec files after rebuild.

### Parameter Types

| Type | Validation | Use Case |
|------|-----------|----------|
| `predicate` | Alphanumeric + underscore | State predicate names |
| `state` | Alphanumeric + underscore | Automaton state names |
| `integer` | Numeric, optional min/max bounds | Bounded values |
| `label` | Non-empty string | Transition labels |
| `expression` | Non-empty string | Free-form mu-calculus sub-expressions |

## Architecture

Templates are compiled into the binary via `include_str!` from the JSON catalog. The `TemplateRegistry` provides lookup, validation, and instantiation. Template resolution happens at the adapter layer (before the emitter/evaluator), producing standard `PropertyFormula::MuCalculus(String)` values.

See also: [CLI Reference](CLI-Reference), [API Reference](API-Reference)
