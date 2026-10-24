# LTL Properties

> **Alpha Software** — Mununu is under active development. APIs, syntax, and behavior may change. We welcome feedback and bug reports via [GitHub Issues](https://github.com/Mumunu-team/mununu/issues).

## What is LTL?

> Source of truth: [`ltl::universal::translate_universal`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/ltl/universal.rs) (`ltl`) and [`ltl::translator::translate_existential`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/ltl/translator.rs) (`eltl`) — surface: CLI+API+UI.

Linear Temporal Logic (LTL) describes properties of reactive systems over infinite execution traces: "every request is eventually acknowledged", "the arbiter never grants both clients", "the bus always returns to idle". An LTL formula **holds on a model when it holds on every path** from the initial states. Mununu translates it into a mu-calculus fixpoint and evaluates that over the CLTS; the translation is exact over the fragment below and refuses the rest rather than check a different property.

## Operator Reference

| Operator | Name | Syntax | Meaning (on a path) |
|----------|------|--------|---------|
| `G` | Globally / Always | `G phi` | `phi` holds at every step |
| `F` | Eventually / Finally | `F phi` | `phi` holds at some step |
| `X` | Next | `X phi` | `phi` holds at the next step |
| `U` | Until | `phi U psi` | `psi` holds at some step and `phi` holds at every step before it |
| `W` | Weak Until | `phi W psi` | `phi U psi`, or `phi` holds forever |
| `R` | Release | `phi R psi` | `psi` holds up to and including the step where `phi` holds, or forever |
| `->` | Implies | `phi -> psi` | If `phi` then `psi` |
| `&&` | And | `phi && psi` | Both hold |
| `\|\|` | Or | `phi \|\| psi` | At least one holds |
| `!` | Not | `!phi` | `phi` does not hold |

**Operator precedence** (highest to lowest): `!` > `G`, `F`, `X` > `U`, `W`, `R` > `&&` > `||` > `->`

Alternative keywords are also accepted: `always` for `G`, `eventually` for `F`, `next` for `X`, `until` for `U`, `weak_until` for `W`, `release` for `R`.

## CTXDSL Syntax

```
mu_formulas {
    formula my_property {
        over MyAutomaton;
        body = ltl G(p -> F(q));      // every path
    }
    formula still_reachable {
        over MyAutomaton;
        body = eltl G(p -> F(q));     // some-path reading (see below)
    }
}
```

### `ltl`: the fragment checked over every path

The formula is brought to negation normal form (`!G p` = `F !p`, `!(p U q)` = `!q W (!p && !q)`) and must then fall in this fragment, where `b` is a propositional formula (atoms with `!`, `&&`, `||`, `->`):

```
phi ::= b | phi && phi | b || phi | X phi | G phi | F b | b U b | b W b | b R b
```

Safety (`G b`), response (`G(b -> F b')`), recurrence (`G F b`), bounded steps (`G(b -> X b')`) and until/weak-until over propositions are all inside it. Outside it, the formula has no exact every-path fixpoint in this form and realization fails naming the subformula:

| refused | why | write instead |
|---|---|---|
| `F G p` (persistence) | eventually-forever is path-wise, not state-wise | mu-calculus, or `eltl F G p` for "some path stabilises" |
| `G p \|\| G q` | a disjunction of two temporal formulas holds path by path | split it, or mu-calculus |
| `F (p && X q)`, `(G p) U q` | `F` / `U` over a temporal formula | mu-calculus |
| `(G F a) -> (G F b)` | after NNF, a temporal disjunction — and it is a fairness assumption, not a property | the fairness templates on the `verify` path (mununu#595) |

`F` and `U` also require a successor at every step, so a path that ends in a deadlock does not reach its goal.

### `eltl`: the some-path reading

`eltl` keeps the branching translation used before 2026-10: `G` and `X` over every successor, `F` and `U` over **some** successor, negation passed through. `eltl F Goal` is `EF Goal` ("`Goal` is reachable"), `eltl G(p -> F q)` is `AG(p -> EF q)` ("the response stays reachable"). This is not LTL's semantics — on a model where the environment may go to `Goal` or loop in `Stuck`, `eltl F Goal` holds and `ltl F Goal` does not — but it is the right claim when "can still get there" is what is meant (mununu#593).

## Property Patterns

### Safety: "Nothing bad ever happens"

```
formula ltl_safety {
    over Tank;
    body = ltl G (Empty || Filling || Full || Draining);
}
```

`G safe` asserts that `safe` holds at every step of every execution.

### Liveness (Response): "Every request is eventually granted"

```
formula ltl_liveness {
    over Tank;
    body = ltl G (Filling -> F Full);
}
```

On every path, every step where the tank is `Filling` is followed by a step where it is `Full`. A path that stalls in `Filling` forever violates it. For "`Full` stays reachable" use `eltl`.

### Recurrence: "Infinitely often"

```
formula heartbeat_check {
    over MySystem;
    body = ltl G F heartbeat;
}
```

`heartbeat` holds infinitely often on every path (`AG AF heartbeat`).

### Liveness under an environment assumption

`ltl (G F a) -> (G F b)` is refused (see the table): an assumption about the environment is not a property of the model. Declare it on the `verify` path — `[[assumptions]]` (`kind = "state" | "edge" | "edges" | "weak"`) with `fair_response` / `fair_always_eventually`, which expand to the fair-`EG` encoding — see [Verify Project Flow](Verify-Project-Flow.md#environment-assumptions--fairness-on-the-verify-path-mununu595).

## LTL to Mu-Calculus Equivalence

| LTL (`ltl`, every path) | Mu-calculus |
|-----|-------------|
| `G phi` | `nu X. (phi && [] X)` |
| `F b` | `mu X. (b \|\| ([] X && <> true))` |
| `X phi` | `[] phi` |
| `b U c` | `mu X. (c \|\| (b && [] X && <> true))` |
| `b W c` | `nu X. (c \|\| (b && [] X))` |
| `b R c` | `c W (b && c)` |
| `G F b` | `nu Y. ((mu X. (b \|\| ([] X && <> true))) && [] Y)` |
| `G(p -> F q)` | `nu Y. ((!p \|\| mu X. (q \|\| ([] X && <> true))) && [] Y)` |

| `eltl` (some-path reading) | Mu-calculus |
|-----|-------------|
| `F phi` | `mu X. (phi \|\| <> X)` |
| `phi U psi` | `mu X. (psi \|\| (phi && <> X))` |
| `G`, `X` | as above |

Key insight: `G` (safety, invariance) maps to **nu** (greatest fixpoint — start with everything, shrink); `F` (liveness) maps to **mu** (least fixpoint — start with nothing, grow).

## When to Use LTL vs. Mu-Calculus

| Use LTL when... | Use mu-calculus when... |
|------------------|------------------------|
| Expressing standard temporal patterns (safety, liveness, response) | You need **labeled modalities**: `< labels = { cs_assert } > Selected` |
| The property follows a well-known pattern (G, F, GF, FG, response) | You need **controllability guards**: `[ (ctrl = controllable) ] X` |
| Readability matters more than expressiveness | You need **nested fixpoints** beyond alternation depth 1 |
| You want automatic translation to mu-calculus | You need fine-grained control over fixpoint variable scoping |
| Writing GR(1) assume-guarantee specs | You are building game-theoretic attractor formulas for synthesis |

LTL is strictly less expressive than mu-calculus -- it cannot express properties that require label-specific modalities or controllability distinctions. For controller synthesis with game semantics, use mu-calculus directly. For standard temporal verification patterns, LTL is more concise and less error-prone.

## See Also

- [Controller Synthesis](Controller-Synthesis.md) -- using LTL/mu-calculus properties for synthesis
- [Hardware Verification Patterns](Hardware-Verification-Patterns.md) -- property patterns for common hardware protocols
- [CLI Reference](CLI-Reference.md) -- evaluating formulas from the command line
