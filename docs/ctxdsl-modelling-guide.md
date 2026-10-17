# Hand-authoring CTXDSL models: what binds, what silently doesn't

CTXDSL's grammar is larger than the fragment its back end evaluates, and the
gap is silent in both directions — an unsupported guard disables a transition
with no error, an unbound atom evaluates to *true* with no warning. Neither
shows up as a failure; both show up as a **wrong verdict**. This page is the
list, every entry verified against the shipped binary.

Read it before hand-authoring a model with `variables`, `guards` or `effects`.
For the language surface itself see
[CTXDSL Language Reference](../wiki/CTXDSL-Language-Reference.md); for the
worked example that motivated this page see
[`examples/verify/v10_mem_fabric_client_mux`](../examples/verify/v10_mem_fabric_client_mux/README.md).

## The one rule that decides your model's shape

> **Transition `guard`s accept exactly ONE comparison. Mu-calculus `formula`s
> accept the full Boolean language.**

Everything below follows from that asymmetry. If your enabling conditions are
conjunctive, either lift the conjunction into the property, push it into the
control state, or generate the model — do **not** write `&&` in a guard.

---

## 1. `&&`, `||` and `!` in a guard silently disable the transition

> Source of truth: [`GuardExpr`](../crates/mununu-core/src/guard/mod.rs#L281),
> [`split_comparison`](../crates/mununu-core/src/guard/mod.rs#L292) — surface:
> CLI+API+UI (every path that realizes a CTXDSL automaton with variables).

The guard parser the unroll path uses
([`parse_guard_expr`](../crates/mununu-core/src/abstraction/unrolling.rs#L1088))
targets a `GuardExpr` with only `True | False | Predicate | Comparison` — no
`And`, `Or` or `Not`. `split_comparison` finds the **first** comparison
operator and takes everything after it as the right-hand side, so

```
guard a == b && a == 2
```

becomes the comparison `a == "b && a == 2"`, which no state satisfies. The
transition is never enabled. There is no error and no warning.

```
// WRONG — this transition never fires
transition S -> S on label g guard (a == b) && (a == 2) effects { h = 1; };

// RIGHT — one comparison per guard
transition S -> S on label g guard a == 2 effects { h = 1; };
```

**Why it matters more than it looks.** A silently-dropped transition is an
*under-approximation*: the model has fewer behaviours than the system. Safety
and reachability verdicts computed on it are unsound in the direction that
hurts — the model reports "not reachable" for a bug that is reachable.

**The three ways out**, in order of preference:

1. **Put the conjunction in the property.** Formulas support `&&`, `||`, `!`,
   variable-to-variable comparison and labelled modalities (§6). This is
   usually possible and always the cheapest.
2. **Encode one conjunct in the control state.** `state Busy` / `state Idle`
   makes "we are in `Busy`" structural, leaving one comparison for the guard.
3. **Generate the model.** Enumerate the semantics in a script and emit
   explicit states — the idiom every shipped memory-domain example uses (§8).

Note that `crate::abstraction::expression::GuardExpr`
([expression.rs:52](../crates/mununu-core/src/abstraction/expression.rs#L52))
*does* have `And`/`Or`/`Not` variants. They are unreachable from CTXDSL source
because the conversion goes through the lossy `crate::guard::GuardExpr` first.
Widening the parser is the natural fix; until then, this page is the contract.

## 2. `effects` are simultaneous and read the pre-state

> Source of truth: [`apply_effects`](../crates/mununu-core/src/abstraction/unrolling.rs#L1258)
> — every effect's right-hand side is evaluated by an `ExpressionEvaluator`
> built on `state` (the pre-state) while writes land in `new_state`. Surface:
> CLI+API+UI.

This is **non-blocking-assignment semantics**, which is what you want when a
transition models a clock edge:

```
variables { var a : i64 = 1; var b : i64 = 0; }
transition S -> D on label go effects { a = b; b = a; };
```

reaches `a = 0, b = 1` — a swap, not `a = 0, b = 0`. Effects in one
transition never see each other's writes, regardless of the order you write
them.

For hardware models this is load-bearing: a transition that carries both an
`accept` (which advances a pointer) and a `grant` (which captures that
pointer) captures the **pre-advance** value, exactly as the RTL would.

## 3. Only `i64` variables bind in formula atoms — `bool` silently evaluates *true*

> Source of truth: [the numeric-valuation gate](../crates/mununu-core/src/context_dsl/realize.rs#L354)
> and [the `IntConstant`-only emit](../crates/mununu-core/src/context_dsl/realize.rs#L3037);
> unknown names reach [`UnknownVariable → GuardResult::Maybe`](../crates/mununu-core/src/abstraction/evaluator.rs#L184).
> Surface: CLI+API+UI.

An unrolled state publishes a per-state valuation for each of its
`IntConstant` variables, and `var == value` atoms bind against those.
`bool`-typed variables emit **no** valuation, so an atom naming one is an
unknown-variable reference, which resolves to `Maybe` and is **included** —
i.e. it evaluates to **true at every state**. So does a misspelled name.

| atom | binds? | value when it doesn't |
|---|---|---|
| `n == 2` where `var n : i64` | yes | — |
| `p == r` between two `i64` vars | yes | — |
| `flag == true` where `var flag : bool` | **no** | **true everywhere** |
| `nosuchvar == 1` (typo) | **no** | **true everywhere** |

Both `flag == true` and `flag == false` return *every* state. A safety
property written over a `bool` is therefore vacuous, and a reachability
property spuriously holds.

**Rules.**

- **Use `i64` for every variable that appears in a property.** Write `var
  busy : i64 = 0;` and compare `busy == 1`, not `bool`.
- **Ship a positive and a negative control with every model.** The failure
  mode is silent-*true*, so a `false` verdict alone does not prove the atom
  channel is live:

  ```
  formula ctl_pos { over A; body = mu X . ((n == 2) || (<> X)); }  // must be TRUE
  formula ctl_neg { over A; body = mu X . ((n == 9) || (<> X)); }  // must be FALSE
  ```

  If the negative control comes back true, an atom in your model is unbound.

- `bool` variables are still fine in `guards` and `effects` — the restriction
  is on formula atoms only.

## 4. Every unrolled copy of an initial location is marked initial

> Source of truth: [`build_clts_from_unrolled`](../crates/mununu-core/src/context_dsl/realize.rs#L3017)
> — `if initial_location_names.contains(&state.location)`. Surface: CLI+API+UI.

`state S initial;` plus `var ptr : i64` unrolls to `S_ptr_0`, `S_ptr_1`,
`S_ptr_2` … and **all** of them are flagged initial. `context eval` then
reports things like

```
Initial states satisfying: 1/3
```

which does not tell you whether the *reset* state is one of the satisfying
ones. (The extra initial states are all reachable from the real one, so for
`EF` and `AG` the aggregate answer is still determined — but you cannot read
it off the count.)

**Idiom: give the model a dedicated reset location** with no incoming edges,
so exactly one unrolled state is initial and the report becomes unambiguous:

```
states { state Reset initial; state S; }
transitions {
    transition Reset -> S on label boot;
    ...
}
```

```
Initial states satisfying: 1/1     ← TRUE from reset
Initial states satisfying: 0/1     ← FALSE from reset
```

## 5. `summarize` shows the declared automaton; `eval` runs the unrolled one

> Source of truth: [`context_summarize`](../crates/mununu-cli/src/main.rs#L7464)
> vs [`context_eval`](../crates/mununu-cli/src/main.rs#L7553) — surface: CLI.

`context summarize` reports `state_count` from the **declared** `states`
block. `context eval` evaluates over the **unrolled** product of states and
variable valuations. The two numbers legitimately differ — a 2-state
automaton with a 0..2 counter summarizes as 2 and evaluates over 4. This is
not a collapse and not a bug; it is two different views. Use `eval`'s state
list, or `--print-structure`, when you need the model the checker actually
saw.

## 6. What formulas support that guards do not

> Source of truth: [Mu-Calculus Reference](../wiki/Mu-Calculus-Reference.md);
> atom binding as in §3. Surface: CLI+API+UI.

Inside a `mu_formulas { formula f { over A; body = … ; } }` body you have:

- `&&`, `||`, `!`
- comparisons against constants (`reg == 2`) **and between variables**
  (`reg == ptr`, `reg < ptr`)
- state names as atomic propositions, and `predicates` declared on the
  automaton
- labelled modalities — `< labels = {accept} > true` holds exactly at states
  with an outgoing `accept` edge; the full guard shape is
  `[(labels = {a}, req_next = {p}, ctrl = controllable)] φ`
- `mu` / `nu` fixpoints

So a conjunctive condition you cannot express as a guard is usually
expressible as a property. Prefer that.

## 7. A predicate naming an unreachable state is a hard error, not `false`

> Source of truth: [`bitset_for_state`](../crates/mununu-core/src/context_dsl/realize.rs#L2277)
> and [`StateNameMatcher::matches_pattern`](../crates/mununu-core/src/context_dsl/state_matching.rs#L23).
> Surface: CLI+API+UI.

`predicates { predicate bad = state Bad; }` resolves by exact name, then by
prefix (so `Bad` matches the unrolled `Bad_ptr_2`). If **no** state matches —
which is what happens when unrolling proves the location unreachable —
realization fails:

```
failed to realise context 'T': unknown state 'Bad' referenced by predicate in automaton 'A'
```

The message says "unknown", but the usual cause is "unreachable". This
matters most for **contrast pairs**: the *sound* variant of a design is
precisely the one where the bug state is unreachable, so the sound model
refuses to load while the broken one answers — the opposite of what you want.

**Two ways to get a `false` instead of an error:**

- **Declare the outcome states unconditionally** (with a self-loop) in every
  variant, so all variants are asked the identical question. This is what
  [`v10_mem_fabric_client_mux`](../examples/verify/v10_mem_fabric_client_mux/README.md)
  does.
- **Use an `i64` flag** rather than a state: `EF (dupf == 1)` over a value
  that is never reached returns `0/N`, cleanly false, with no error.

## 8. Composition: declare `controllable` explicitly, or shared labels collide

> Source of truth: [`classify_transition_controllability`](../crates/mununu-core/src/context_dsl/realize.rs#L3143)
> — surface: CLI+API+UI.

If an automaton declares **no** `controllable { … }` / `internal { … }` block,
label classification falls back to legacy inference and every label it uses
becomes **Controllable**. Two automata sharing a label then both claim
ownership:

```
failed to realise context 'T': controllable alphabet element 'label 'tick'
is controllable in both 'P' and 'Q'' already claimed by another CLTS
```

Declaring the block — **even empty** — switches to explicit classification,
and anything not listed becomes Uncontrollable and may be shared:

```
automaton P { controllable { }  /* everything is environment-driven */ ... }
```

Also remember synchronous composition needs a **per-automaton skip label**;
two automata sharing one idle label are forced to idle together. Variables
from both members bind in formulas over the product.

## 9. `verify.toml`: template `TARGET`s are identifiers; use `formula` for expressions

> Source of truth: [`validate_param_value`](../crates/mununu-core/src/adapter/templates/mod.rs#L305)
> (`ParamType::Predicate | ParamType::State` must be alphanumeric + `_`) and
> [`PropertySpec::formula`](../crates/mununu-core/src/verify/config.rs#L307).
> Surface: CLI+API.

```toml
# REJECTED: template instantiation failed: parameter 'TARGET':
#           expected identifier (alphanumeric + underscore), got 'n == 2'
[[properties]]
template = "reachable"
args = { TARGET = "n == 2" }

# WORKS — the raw formula field takes the full expression language
[[properties]]
formula = "mu X . ((n == 2) || (<> X))"
```

`var == value` atoms bind through the `verify` path for a **single-source**
project. They do **not** bind across a multi-source composition — see the
matrix below.

### Where `var == value` atoms bind

Verified 2026-09-04, positive and negative control per cell.

| path | binds? |
|---|---|
| `context eval`, one automaton | yes |
| `context eval`, in-file `composition { synchronous … }` of two variable-bearing automata | yes |
| `verify.toml`, one `[[sources]]` entry, via `formula = "…"` | yes |
| `verify.toml`, **two or more** `[[sources]]` composed (either semantics) | **NO — every variable atom returns `false`** |

The multi-source case is the dangerous one, because it fails *closed* for
real variables (`false`) while an unknown name still returns `true`:

```
pos_both_two      (pc == 2 && qc == 2)   False   ← reachable; should be True
neg_skew          (pc == 2 && qc == 0)   False
ctl_state_reach   reachable(P0)          True    ← state names bind fine
ctl_typo_atom     (nosuchvar == 7)       True    ← unknown atoms still true
```

A negative control cannot detect this — both come back `false`. **A positive
control can, and is the reason §3 asks for both.**

**So for a multi-source `verify` project, do not put variable atoms in
properties.** Encode the outcome as a state and use `reachable(<StateName>)`;
state-name atoms bind on every path. This is why the shipped multi-model
examples are generated with the configuration in the state name.

## 10. Choosing between `variables` and explicit states

| Use `variables` + `guards` + `effects` when | Generate explicit states when |
|---|---|
| each transition's enabling condition is a **single** comparison | enabling conditions are conjunctive |
| the data is a counter or a small integer you want to compare in properties | the configuration is a tuple of interacting fields |
| the state space is small and you can bound every variable by a guard | you need exact control over what coincides in one step |
| you want `var == value` atoms in your properties | you want `reachable(NamedOutcome)` templates |

An unbounded variable enumerates until it hits the state cap and answers
nothing, so **bound every variable** with a guard on the transition that
increments it. `i64`, `bool` and enum names are the only types
([`parse_type_name`](../crates/mununu-core/src/context_dsl/parser.rs#L598));
there is no range-typed variable, so bounds come from guards or from
structure.

The shipped memory-domain examples —
[`v1_noc_mesh_4router`](../examples/verify/v1_noc_mesh_4router/) (variables,
one comparison per guard),
[`v2_tso_storebuffer`](../examples/verify/v2_tso_storebuffer/) and
[`v10_mem_fabric_client_mux`](../examples/verify/v10_mem_fabric_client_mux/)
(both generated, zero variables) — are the two poles.

## 11. Known documentation drift

Verified 2026-09-04 against the shipped binary. Fixed where noted.

| Claim | Status |
|---|---|
| `wiki/CTXDSL-Language-Reference.md` "Variables" showed `count: i64 = 0;` | **wrong syntax** — the `var` keyword is required. Fixed. |
| `wiki/Property-Templates.md` documents `mununu templates` and `context eval --template` / `--template-arg` | **NOT drift — these all exist.** A 2026-09-04 revision of this page wrongly called them missing; that was read off a stale local binary and is retracted. Corrected 2026-09-05. |
| `examples/verify/v2_tso_storebuffer` README/generator: "`r0 == 0` atoms do not bind through the verify composition path" | **correct for multi-source `verify`, over-broad as written** — atoms bind on `context eval` and on single-source `verify` (which is that example's own shape). Scoped. |

**A note on how this page is verified, added after getting one wrong.** Every
claim here is checked two ways: run against a **freshly built** binary, and
confirmed in the source with a `Source of truth:` anchor. The retracted row
above failed the first check — it was run against a month-old `target/debug`
build that predated the subcommand, and the source was never consulted because
the CLI answer looked conclusive. A `--help` that lacks a subcommand is
evidence about **your binary**, not about the tool. Rebuild before you file
drift.

---

# Part II — modelling traps: what your abstraction asserts

Everything above is about what the **tool** does silently. These two are about
what your **model** claims. Neither is a language feature or a bug; both are
choices that look like ordinary abstraction and are not, and each one cost a
real design real time (mununu#496, mununu#497).

They share a shape with the composition gotcha in
[Composition](../wiki/Composition.md#key-gotchas) — *a shared label the component
cannot take is a label the environment cannot emit, silently.* In all three
cases the model does not **miss** the bug. It **assumes it away**, and then
reports a clean verdict about a system that is not yours.

## A. The atomic-action trap

**An action modelled as one transition asserts that every resource it uses is
held, unchanged, for exactly that step.**

A `fill` label meaning *"the fetcher lands this line's row into the stage"* is a
single transition. Nothing can happen "during" it, because there is no during —
so a payload that changes mid-fill is not a behaviour the model failed to
explore. It is a behaviour the model declared impossible.

> **The rule: if the RTL can do anything between the start and the end of your
> label, your label is wrong.**

The remedy is a pattern, not a feature. Any resource with a handshake wants
**`acquire → use* → release`** as separate labels, with the payload's identity
carried in the automaton's *state* across the `use*`:

```
// WRONG — one transition, so "during the fill" does not exist.
automaton Atomic {
    states { state Idle initial; state Done; }
    transitions {
        transition Idle -> Done on label fill;
        transition Done -> Done on label sink;
    }
}

// RIGHT — the fill is open across the writes, and `src` records WHICH payload
// the line was acquired for, so a mid-fill change is expressible.
automaton Handshake {
    controllable { }
    variables { var src : i64 = 0; var live : i64 = 0; }
    states { state Reset initial; state Free; state Held; }
    transitions {
        transition Reset -> Free  on label boot;
        transition Free  -> Held  on label acquire effects { src = live; };
        transition Held  -> Held  on label write;
        transition Held  -> Free  on label release;
        // The environment may re-point the payload mid-fill.
        transition Held  -> Held  on label payload_change effects { live = 1; };
        transition Free  -> Free  on label payload_change effects { live = 1; };
    }
}
```

The torn fill is now a reachable state rather than an inexpressible one:

```
formula torn_fill { over Handshake; body = mu X . ((Held && (src != live)) || (<> X)); }
formula ctl_neg   { over Handshake; body = mu X . ((src == 9)             || (<> X)); }
```

```
torn_fill   Initial states satisfying: 1/1     <- the bug is expressible, and reachable
ctl_neg     Initial states satisfying: 0/1     <- and the atom channel is live (§3)
```

In the `Atomic` automaton there is no formula that states this property at all —
which is the tell. **If you cannot write the negative, you have not abstracted
the bug away, you have defined it out of the language.**

## B. `N = 1` is not an abstraction of `N` — it is a different system

**Collapsing a multiplicity to one deletes the entire class of "two parties
disagree about which one".**

A model with one implicit slot cannot state *two words disagreeing about which
slot they belong to*, because stating it needs at least two slots. Reducing `N`
to 1 feels like ordinary abstraction — the same move as dropping a data width —
and it is not the same move at all: it removes most of what a shared-resource
model is **for**.

> **`N = 2` is the smallest honest number wherever identity matters.** One
> slot models a resource; two model a resource people can disagree about.

The cost is usually small — a second slot squares a small state space, not a
large one — and the payoff is the whole mis-routing / mis-pairing / mis-arbitration
class. If you genuinely only ever have one, say so as a *comment declaring the
assumption*, so a later reader knows the model is silent on identity rather than
clean about it.

This is why the worked example in
[`v10_mem_fabric_client_mux`](../examples/verify/v10_mem_fabric_client_mux/README.md)
carries a four-word stream rather than a one-word one: a duplicate needs two
words to state, and a skip needs a dropped word *and its successor*.

## What CTXDSL correctly does not do

Recorded so the technique is not oversold. **CTXDSL models carry no data**, so a
data-placement fault — a bank shifted by one halfword, a byte lane swapped, an
endianness flip — is out of scope, and no amount of modelling discipline will
catch it here. That is not a gap to close: dropping data is exactly what makes
these models cheap enough to write *before* the RTL, which is where their value
is. Data-placement faults want a different instrument — rebuilding the image
from the design's own returns and diffing it against the source, or the
structural `sv lint` rules in
[`verifying-rtl.md`](verifying-rtl.md#preflight-sv-lint--structural-faults-refused-at-write-time).

---

## Checklist before trusting a hand-authored model

1. No `&&` / `||` / `!` inside any `guard`. (§1)
2. Every variable appearing in a property is `i64`. (§3)
3. A positive control atom returns true and a negative control returns false. (§3)
4. Every variable is bounded by a guard, or encoded structurally. (§10)
5. A dedicated reset location, so initial-state counts read `k/1`. (§4)
6. A non-vacuity gate: the "good" outcome is reachable. (§7)
7. For a contrast pair, both variants declare the same outcome states and are
   asked the identical property. (§7)
8. A control that *removes* the mechanism you believe causes the bug, and
   makes the verdict flip. (see
   [`v10`'s strict-schedule control](../examples/verify/v10_mem_fabric_client_mux/README.md))
9. No label spans an interval in which the RTL can act. (Part II §A)
10. Any multiplicity where identity matters is at least 2. (Part II §B)
