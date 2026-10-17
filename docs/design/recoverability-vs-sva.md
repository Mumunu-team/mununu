# Recoverability vs SVA — why "can it always get back?" is not a linear property

> Status: planning — positioning doc for the Track-B recoverability showcase. The
> capability sections carry `> Source of truth:` anchors against live code; the
> conceptual sections are tagged `> Concept:` per CLAUDE.md §Documentation
> Traceability.

This note explains, precisely and with a worked example on real OpenTitan RTL,
one capability gap that is structural rather than incidental: **SystemVerilog
Assertions cannot state recoverability** — "from every reachable state, can the
design still get back to a good state?" — and mununu can, soundly, over a
predicate abstraction. It also records a second, narrower finding from the
Track-H viability spike: with the open-source toolchain, even *extracting* a
design's existing SVA into a model checker's input is blocked, which sharpens
where mununu's value actually sits (the model, plus properties SVA can't write —
not re-checking the SVA itself).

## 1. The property: recoverability is `AG EF good`

> Concept: recoverability as a branching, alternating-fixpoint property.

"Can the system always return to a good state?" is the CTL formula `AG EF good`:
from every reachable state (`AG`), there exists a path back to `good` (`EF`). In
the modal mu-calculus this is an **alternating fixpoint** — a greatest fixpoint
(the `AG`/safety envelope) wrapping a least fixpoint (the `EF`/reachability
core):

```
always_recoverable  =  nu Y. ((mu X. (good || <> X)) && [] Y)
```

The inner `mu X. (good || <> X)` is `EF good` ("some successor path reaches
good"); the outer `nu Y. (… && [] Y)` closes it under "and this holds at every
reachable state." The `<>` (some-successor) inside an `[]` (all-successors) is
the branching content: it quantifies existentially over futures *inside* a
universal envelope.

## 2. Why SVA cannot express it

> Concept: linear (LTL/SVA) vs branching (CTL/mu-calculus) expressiveness.

SVA's temporal layer is **linear-time**: an `assert property` constrains every
individual execution trace, one at a time. `EF good` is not a property of a
single trace — it asserts the *existence of a branch* to `good` from a state the
actual run may never take that branch from. A linear formalism can say "on this
trace, good eventually holds" (`F good`); it cannot say "from wherever this trace
is now, a path back to good *exists*," because that quantifies over branches the
trace did not take. `AG EF good` lives strictly outside LTL, hence outside SVA.

The usual SVA workaround is the **shadow-logic tax**: the engineer adds
non-functional modeling state — a "stuck" monitor FSM, liveness flags, `cover`
witnesses for individual recovery paths — and asserts linear properties over that
shadow logic. This (a) only ever checks the *specific* recovery paths the
engineer thought to instrument, never "a path exists from every state," and (b)
adds RTL that must itself be reviewed, simulated, and kept in sync with the
design. Recoverability as a single branching question over the *unmodified*
design is not available in that workflow.

`cover property (good)` is the closest native SVA gets: it asks "is `good`
reachable on some trace from reset" — that is `EF good` from the initial state
only, **not** `AG EF good`. A design can satisfy every `cover` and still have an
absorbing error state from which `good` is unreachable; `cover` never visits that
state's branch. This is exactly the gap Track I.3 proposes to close
automatically (upgrade a verified `cover`/`EF` to its `AG EF` companion and report
the discriminating case).

## 3. What mununu does instead

### 3.1 Sound `AG EF` over a predicate abstraction

> Source of truth: [`predicate_cube_lift`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs#L2440) + [`cegar_refine_loop`](../../crates/mununu-core/src/adapter/btor2/cegar.rs#L697) — surface: (CLI+API+UI)

mununu lifts a BTOR2 design into a **predicate cube** (abstract states are
elements of `2^|P|` over a declared predicate set `P`, not the bit-blast state
cross-product), evaluates the full modal mu-calculus over it, and refines on an
undecided verdict. The `always_recoverable` formula above is evaluated directly —
the alternating fixpoint is not flattened or approximated away.

The abstraction is sound for the branching property by the Bruns–Godefroid
(CONCUR 2000) 3-valued preservation theorem: definite verdicts (`KleeneT`,
`KleeneF`) transfer to the concrete design at *every* alternation depth, νμ
included. The third value (`KleeneBot`) means "the abstraction cannot decide" and
triggers refinement rather than a wrong answer.

> Source of truth: [`KleeneDom`](../../crates/mununu-core/src/mu_calculus/evaluator.rs#L1315) — surface: (CLI+API+UI)

A definite `AG EF` answer needs *must*-edges (an under-approximation of the
transition relation) for the inner `EF`, supplied by SMT-proved must / hyper-must
inference rather than sampling:

> Source of truth: [`MustEdgeInference::SmtHyperMust`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs#L547) — surface: CLI (`--must-edge-inference smt-hyper-must`)

The verdict, soundness, and the audited modal fragment it holds over
(`Control::All`, bare `<>`/`[]`, unbounded) are developed in
[`kmts-theory.md`](kmts-theory.md) §7.

### 3.2 Worked example — OpenTitan `csrng_main_sm`

> Source of truth: [`examples/verify/v7_csrng_recoverability/validate.sh`](../../examples/verify/v7_csrng_recoverability/) — surface: CLI

V.7-c runs `always_recoverable` with `good = (state_q == MainSmIdle)` on the real
OpenTitan `csrng_main_sm` FSM (vendored under the M.2 fixture), via
`sv2v → yosys → btor2 cegar … --must-edge-inference smt-hyper-must`. The verdict
is *definite both ways and depends on reset*, which is the honest and interesting
result:

- **Reset available** (`rst_ni` a free input): `AG EF idle` is **definite-TRUE**
  — asserting reset returns the FSM to `MainSmIdle` from any state.
- **Reset held inactive** (`connect -set rst_ni 1'b1`): `AG EF idle` is
  **definite-FALSE** (`T=0 / F=4`) — `MainSmError` and the unreachable sparse
  encodings become permanent traps once reset is withheld.

This is a branching question SVA cannot phrase, answered with sound definite
verdicts on production RTL. The Track-I countertrace surfaces the trap directly:
the reset-held run reports `counterexample trace (1 step, ends in trap):
{idle=false, err=false}` — the initial cube is itself the trap.

> Claims-integrity: V.7-c is a *design-pattern demonstration* on real RTL, not a
> vulnerability finding — the reset-dependence it surfaces is intended SEC_CM
> behaviour. The only finding-grade anchor mununu has is the Caliptra
> `soc_ifc_boot_fsm` CWE-1245 pair.

## 4. A second finding: extracting existing SVA is itself blocked (open toolchain)

> Concept: the open-source SV → BTOR2 frontend boundary for assertions.

The Track-H viability spike asked whether the existing pipeline can *auto-verify
a design's own SVA*. The back half works and ships today: mununu's BTOR2 parser
reads `bad` / `justice` / `constraint` / `fair`, and the bit-blast lowering
auto-translates them to mu-calculus with no formula input — a `bad` becomes
`nu X. ((!pred) && ([] X))` (i.e. `AG ¬bad`):

> Source of truth: [`bit_blast.rs` bad→safety / justice→liveness](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs#L2961) — surface: (API via verify)

So any BTOR2 *that already carries properties* (HWMCC benchmarks, or anything a
Verific-backed flow emits) is auto-verifiable. The blocker is upstream, in the
open frontend:

- Open Yosys `read_verilog -sv` does **not** parse concurrent `assert property
  (@(posedge clk) …)` (syntax error at `@`), nor temporal operators (`|->`,
  `$stable`, `$past`). Those need the commercial Verific frontend
  (`read_systemverilog`).
- `sv2v` (the normalizer OpenTitan RTL requires before Yosys can read it)
  **silently drops** concurrent assertions — a normalized `prim_arbiter_fixed`
  emerges with zero assertions.
- OpenTitan's real properties are concurrent and temporal (`Priority_A` uses
  `|->`; `CsrngMainErrorStStable_A` uses `|=> $stable`), so only assertion-free
  immediate `assert(expr)` would survive — which OpenTitan does not write.

The consequence is a two-part plan. The differentiated *power* is not re-checking
a design's existing SVA (linear properties mature BMC/IC3 tools already handle).
But extracting that SVA is still worth doing — as the *means* to the
differentiator: the design's own `cover` / reachability properties are exactly the
seeds for the `EF` → `AG EF` recoverability upgrade (§2, Track I.3), surfaced
automatically with no hand-authored properties. Since the open frontend can't
deliver the SVA, the planned mechanism is a **custom SVA → mu-calculus translator**
that parses the SVA from source and emits mu-calculus bound to the auto-extracted
model (plan + coverage criteria + tests:
`.claude/plans/sva-to-mucalculus-translator-2026-06-25.md`). So mununu's value is
(1) extracting the *model* (works today; V.7-c proves it), (2) ingesting the
design's SVA via the custom translator for a plug-and-play endpoint, and (3) —
the differentiator — upgrading the reachability properties to the branching
recoverability/realizability questions SVA cannot express.

## 5. Where this leads

- Track B ships the recoverability showcase (V.7) as the worked evidence for §3.
- The **custom SVA → mu-calculus translator** (phases XL.0–XL.7) is the
  open-toolchain SVA-extraction path and the host for the I.3 upgrade
  (`.claude/plans/sva-to-mucalculus-translator-2026-06-25.md`).
- Track I.3 turns §2's `cover`→`AG EF` gap into an automatic check on extracted
  designs — implemented as translator phase XL.2 (form + check `AG EF` for each
  TRUE `cover`/`EF`, with the Track-I countertrace on failure).
- The Track-H gate (`.claude/plans/sva-auto-h0-gate-verdict-2026-06-25.md`)
  records §4's spike in full; the SVA-extraction criterion is reinstated, now
  scoped to the translator's coverage tiers + an honest coverage report.

See also [`industrial-value-and-validation-domains.md`](industrial-value-and-validation-domains.md)
for the broader domain map and [`kmts-theory.md`](kmts-theory.md) for the
soundness theory.
