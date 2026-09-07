# LTL Properties

> **Alpha Software** — Mununu is under active development. APIs, syntax, and behavior may change. We welcome feedback and bug reports via [GitHub Issues](https://github.com/Mumunu-team/mununu/issues).

## What is LTL?

Linear Temporal Logic (LTL) is a specification language for describing properties of reactive systems over infinite execution traces. For hardware verification engineers, LTL provides a concise way to express timing relationships between signals -- "every request is eventually acknowledged," "the arbiter never grants both clients simultaneously," or "the bus always returns to idle." Mununu compiles LTL formulas into mu-calculus fixpoints and evaluates them over Compositional Labeled Transition Systems (CLTS), so you get the readability of LTL with the full power of mu-calculus model checking.

## Operator Reference

| Operator | Name | Syntax | Meaning |
|----------|------|--------|---------|
| `G` | Globally / Always | `G phi` or `G(phi)` | `phi` holds in every state along every path |
| `F` | Eventually / Finally | `F phi` or `F(phi)` | `phi` holds in some future state along every path |
| `X` | Next | `X phi` or `X(phi)` | `phi` holds in the immediate next state |
| `U` | Until | `phi U psi` | `phi` holds continuously until `psi` becomes true (and `psi` must eventually hold) |
| `W` | Weak Until | `phi W psi` | `phi` holds until `psi` becomes true, or `phi` holds forever |
| `R` | Release | `phi R psi` | `psi` holds until `phi` releases it (dual of Until) |
| `->` | Implies | `phi -> psi` | If `phi` then `psi` |
| `&&` | And | `phi && psi` | Both `phi` and `psi` hold |
| `\|\|` | Or | `phi \|\| psi` | At least one of `phi` or `psi` holds |
| `!` | Not | `!phi` | `phi` does not hold |

**Operator precedence** (highest to lowest): `!` > `G`, `F`, `X` > `U`, `W`, `R` > `&&` > `||` > `->`

Alternative keywords are also accepted: `always` for `G`, `eventually` for `F`, `next` for `X`, `until` for `U`, `weak_until` for `W`, `release` for `R`.

## CTXDSL Syntax

LTL formulas are written inside `mu_formulas` blocks using the `body = ltl ...;` prefix:

```
mu_formulas {
    formula my_property {
        over MyAutomaton;
        body = ltl G(p -> F(q));
    }
}
```

The `ltl` keyword tells the parser to interpret the formula body as LTL rather than mu-calculus. The formula is automatically translated to an equivalent mu-calculus expression before evaluation.

## Property Patterns

### Safety: "Nothing bad ever happens"

A safety property asserts that some condition holds in every reachable state. If violated, there exists a finite counterexample trace.

```
formula ltl_safety {
    over Tank;
    body = ltl G (Empty || Filling || Full || Draining);
}
```

The pattern `G safe` asserts that `safe` is an invariant -- it holds at every step of every execution.

### Liveness (Response): "Every request is eventually granted"

A liveness property asserts that something good eventually happens. The response pattern `G(p -> F(q))` guarantees that whenever trigger `p` holds, response `q` will eventually follow.

```
formula ltl_liveness {
    over Tank;
    body = ltl G (Filling -> F Full);
}
```

This says: whenever the tank is `Filling`, it will eventually reach `Full`.

### GR(1) Fairness Response: "Repeated stimulus yields repeated response"

Generalized Reactivity(1) properties combine liveness guarantees under fairness assumptions. The pattern `G(p -> F(q))` also serves as a GR(1) response when both `p` and `q` recur.

```
formula ltl_response {
    over Tank;
    body = ltl G (Full -> F Empty);
}
```

This says: every time the tank reaches `Full`, it will eventually return to `Empty` -- the system cycles fairly.

### Persistence: "Eventually stable forever"

The persistence pattern `F G phi` asserts that `phi` eventually becomes true and remains true for all subsequent states. Useful for convergence properties.

```
formula eventually_stable {
    over MySystem;
    body = ltl F G stable;
}
```

### Recurrence: "Infinitely often"

The recurrence pattern `G F phi` asserts that `phi` holds infinitely often -- the system keeps revisiting a condition. Useful for heartbeat, watchdog, and liveness checks.

```
formula heartbeat_check {
    over MySystem;
    body = ltl G F heartbeat;
}
```

## LTL to Mu-Calculus Equivalence

When Mununu compiles an LTL formula, it produces an equivalent mu-calculus fixpoint expression. The translation rules are:

| LTL | Mu-Calculus | Fixpoint Type |
|-----|-------------|---------------|
| `G phi` | `nu X. (phi && [] X)` | Greatest fixpoint (nu) |
| `F phi` | `mu X. (phi \|\| [] X)` | Least fixpoint (mu) |
| `X phi` | `[] phi` | Box modality (one step) |
| `phi U psi` | `mu X. (psi \|\| (phi && [] X))` | Least fixpoint (mu) |
| `phi W psi` | `(phi U psi) \|\| G phi` | Mu + Nu combined |
| `phi R psi` | `!((!phi) U (!psi))` | Negated least fixpoint |
| `G F phi` | `nu Y. (mu X. (phi \|\| [] X) && [] Y)` | Nested nu-mu |
| `F G phi` | `mu Y. (nu X. (phi && [] X) \|\| [] Y)` | Nested mu-nu |
| `G(p -> F(q))` | `nu X. ((!p \|\| mu Y. (q \|\| [] Y)) && [] X)` | Nested nu-mu |

Key insight: `G` (safety, invariance) maps to **nu** (greatest fixpoint -- start with everything, shrink). `F` (liveness, reachability) maps to **mu** (least fixpoint -- start with nothing, grow). The alternation depth of nested fixpoints determines the complexity class of the property.

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
