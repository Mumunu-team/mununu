# Consumer briefing — `ltl` is checked over every path; `eltl` keeps the some-path reading; unparseable formulas are refused (2026-10)

> Source of truth: [`translate_universal`](../../crates/mununu-core/src/ltl/universal.rs) + [`RealizationError::InvalidFormula`](../../crates/mununu-core/src/context_dsl/realize.rs) — surface: (CLI+API+UI) — every path that realizes a CTXDSL document (`context eval|synth|graph|summarize`, `verify`, `/api/v1/context/*`), and the Promela import (its `ltl` claims are emitted as `body = ltl …`)

**Audience:** monono, mununu-ui and anyone who writes `body = ltl …` or imports Promela models. Fixes mununu#764.

## TL;DR

- **Verdict change (soundness fix).** `ltl` read `F` and `U` existentially (`F p` was `EF p`) and passed `!` through, so an LTL property was checked over SOME path. It is now checked over EVERY path, LTL's semantics. Formulas that held only because some path satisfied them now come back violated.
- **New error.** The every-path translation is exact on a fragment (`G`, `X`, `∧`, `b ∨ φ`, `F b`, `b U b`, `b W b`, `b R b` with `b` propositional — safety, response `G(b -> F b')` and recurrence `G F b` included). Outside it (`F G p`, `G p || G q`, `(G F a) -> (G F b)`) realization fails naming the subformula.
- **New keyword.** `body = eltl …;` keeps the previous some-path translation, for "the goal stays reachable" claims (mununu#593).
- **New error.** A formula that does not parse or translate is a realization error (`formula 'X' cannot be evaluated: …`). It used to be evaluated as `true` and held.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | none (RTL only) | none |
| **monono** | `context eval` / `verify` on models with `ltl` bodies: `F` / `U` properties may flip holds → violated; out-of-fragment `ltl` and malformed formulas now fail to realize | re-run the CTXDSL lanes; move "reachable" claims to `eltl`, out-of-fragment ones to μ-calculus |
| **mununu-ui** | the same verdicts and errors over `/api/v1/context/*`; the editor should accept `eltl` | add `eltl` to the CTXDSL highlighter |
| Promela importers | imported `ltl` claims are checked over every path (SPIN's reading); a claim outside the fragment fails the import | write it as μ-calculus in a sidecar |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the translation lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | no | RTL image |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | no | rosf does not realize CTXDSL |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | CTXDSL model lanes |

## Measured

| run | result |
|---|---|
| `an_ltl_eventually_must_hold_on_every_path` (`context/mod.rs`) | `ltl F Goal` with a `Stuck` branch: satisfied at `Init` on the parent, violated here; `eltl F Goal` satisfied |
| `ltl::universal` unit tests | 12 in-fragment formulas translate; `F G p`, `G p \|\| G q`, `F (p && X q)`, `(G p) U q`, `!(G F p)` are refused |
| `formula_referencing_a_3valued_predicate_realizes` | was passing on the `true` fallback (its body `"boot_idle"` never parsed); the body is now `boot_idle` and the test checks what it says |
| shipped models (`examples/**`, fixtures, mununu-ui `public/examples/**`) | 153 realized with `context summarize`: 0 refused |
| `cargo test -p mununu-core --no-fail-fast` | all 55 test binaries pass |

## Provenance

- Fix commit: pending merge — branch `fix/ltl-universal-semantics`.
- Docs: [`wiki/LTL-Properties.md`](../../wiki/LTL-Properties.md).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- Full LTL over every path (an automaton product) for the formulas outside the fragment.
- The TLSF / GR(1) game translation (`adapter/emit.rs`, turn-guarded system boxes) is unchanged.
