# Where mununu's *owned* engines fail on HWMCC'20 bv — a categorized gap analysis

> Grounded in the full-portfolio run (`~/hwmcc20-bench/results-full/FINAL.md`,
> `verify-full.log`, 2026-07-11): `mununu btor2 verify` over all 136 HWMCC'20 bv
> instances, 120 s outer timeout shared across 5 engines. **Owner note (by *algorithm*, not
> deployment):** the exact (BDD), native word-level BMC/k-induction, and native McMillan
> interpolation engines are **mununu-owned** (mununu drives the search; z3/cvc5 are per-query
> oracles). **`native_spacer` is NOT** — it builds a btor2→CHC encoding and hands the whole
> IC3/PDR search to **Z3's SPACER** (an external algorithm, run in-process via the linked z3
> library), the same algorithm-ownership class as the two subprocess checkers **btormc** and
> **Pono**.

## The raw split

| Decider | Count (of 41 decided) | Algorithm owner |
|---|---|---|
| Pono (safety proof) | 23 | **external** (subprocess) |
| btormc (counterexample) | 17 | **external** (subprocess) |
| Z3 SPACER (via mununu's CHC encoding) | 3 | **external algorithm** (Z3, in-process; mununu owns only the encoding) |
| exact BDD | 1 | **mununu** |
| native BMC/k-induction | 1 | **mununu** |

**mununu-owned model-checking algorithms decide only 2/41** (exact 1, native BMC/k-ind 1);
the other 39 are external algorithms — Z3 SPACER (3), btormc (17), Pono (23). ~88/136 are
undecided by anything. So on this suite the mununu-owned engines are *not* the workhorse —
they contribute the exact/soundness cross-check and, via native McMillan interpolation, a
few unique decides (see the `gen12/14/39` note). (`native_spacer` is a **Z3 SPACER
frontend**, not a mununu algorithm — mununu builds the btor2→CHC encoding, Z3 runs the
IC3/PDR search; it is counted external here.)

## First, the honest framing: HWMCC bv is uniformly one property class

Every bv-track obligation is **bit-level safety** — `AG ¬bad` (is a `bad` node reachable?).
So the owned-engine failures here are **not** by *property class*; they are by **model
characteristic**. The property is always the same; what changes is the design's width,
sequential depth, memory content, and datapath arithmetic. The categories below are those
characteristics. (The property-class gap — liveness / branching — is a separate axis the bv
suite does not exercise; see the last section.)

## The categories owned engines fail on, and why

### A. State space ≫ 40 bits → the exact engine is structurally excluded

mununu's flagship **exact** 3-valued BDD engine is hard-capped at 40 register+input bits
(`MAX_BITBLAST_BITS`, `symbolic_bitblast.rs`) and `Skip`s above it. HWMCC designs are not
close to that ceiling:

| Instance | Total state bits |
|---|---|
| `arbitrated_top_n2_w8_d16_e0` (the *narrowest*) | **313** |
| `arbitrated_top_n2_w64_d64_e0` | 8,323 |
| `arbitrated_top_n2_w128_d32_e0` | 8,378 |

Even the "w8" arbiter is 313 bits — ~8× the cap. The exact engine's only bv decide
(`paper_v3`) is a 644-byte toy. **Why it fails:** BDD reachability is exponential in the
BDD size, which explodes on hundreds-to-thousands of state bits; the cap is a deliberate
abstain (sound) rather than an OOM. **Consequence:** the exact engine — the one that gives
mununu its "definite verdict, sound at every alternation depth" guarantee — contributes
essentially nothing on real bit-vector benchmarks. Above 40 bits you are on the abstraction
cube or the external tools.

### B. Non-trivially-inductive safety proofs → native k-induction + the Z3-SPACER-via-CHC path ≪ Pono's IC3/PDR

Most safe HWMCC instances need an **auxiliary strengthening invariant** — the property is
not k-inductive for small k. Generating that invariant *is* modern model checking.

- **Examples (Pono proves safe, owned engines miss):** `cal159`, `cal161`, `cal162`,
  `cal21`, `cal35`, `cal37`, `cal33`, `cal4`, `cal41`, `gen10/12/14/21/35/39`,
  `elevator.4`. The `cal*` family (200+ instances) is almost entirely Pono-or-nothing.
- **Examples (nobody decides):** the deep `arbitrated_top_*_d64/d128` proofs.

**Why it fails:** the truly-owned **k-induction** only closes when the property is inductive
at the tried depth — it does not synthesize the missing invariant. The `native_spacer` path
(mununu's btor2→CHC encoding, decided by **Z3's SPACER** — not a mununu algorithm) encodes
the design as a *single* `Inv` relation; run in-process on a shared budget it underperforms
a dedicated **Pono** `ic3bits`, which does incremental inductive generalization (CTI blocking
+ MIC) tuned for bit-level transition systems. Note this gap is *not* fixed by any
mununu-owned invariant discovery — mununu's own IC3ia foundation (`abs_safety.rs`) abstains
on real designs for the same reason. **This is the single biggest gap on HWMCC:
safe-instance proofs at scale need a strong external IC3/PDR (Z3 SPACER or Pono); mununu's
owned safety algorithms do not synthesize the strengthening invariant.**

### C. Deep counterexamples → btormc's optimized BMC out-reaches native BMC in budget

For *violated* instances, native BMC is correct (it only ever claims `Violated`) but slow.

- **Examples (btormc finds the CEX, owned miss):** `arbitrated_top_*_d16` (several),
  `circular_pointer_top_w*_d*`, `brp2.3`, `at.6`, `anderson.3`.

**Why it fails:** btormc/Boolector's BMC uses incremental SAT with a mature bit-vector
encoding and reaches the violation depth quickly; mununu's native BMC (word-level, z3
per-depth) is far less optimized and times out before the CEX depth within the shared 120 s.
The gap is *engineering throughput on deep unrollings*, not a soundness or expressiveness
gap — the owned BMC would find the same CEX given enough time.

### D. Arrays / memories → BDD explodes *and* the CHC array encoding is unsound-suspect

Array-backed designs (`vcegar_arrays_*`, `vis_arrays_*`) make each memory word part of the
state, so the model is both wide (Category A) and semantically array-heavy.

- **Example (the one confirmed defect):** `vcegar_arrays_itc99_b12_p2` — mununu's native
  SPACER returned **`violated`** where the HWMCC ground truth is **safe** (`unsat`), and no
  other engine (native BMC, btormc BMC to depth 200/300) could corroborate the claimed CEX.
  Preponderance of evidence: a **spurious counterexample from mununu's btor2→CHC encoding**
  (Z3's SPACER itself is sound; the suspect is the encoding). Flagged, not 100 % confirmed.
- **The second one (2026-10-19, mununu#740):** `vis_arrays_am2901` — SPACER (libz3 4.13.3, the
  build the images link) returned **`safe`** where the HWMCC ground truth is **violated at depth
  16** (six competition solvers; rIC3 prints the witness in 0.2 s). This time the ENCODING was
  cleared: the native BMC on the same `encode_design` view, with the inputs pinned to the
  witness, finds the violation in 0 s, and the same CHC on z3 4.16.0 is `unknown` — a wrong
  `unsat` from that z3 build. Since 2026-10-20 a lone SPACER verdict in EITHER direction
  decides nothing (`reach_portfolio::collect`); it corroborates, and the re-promotion criterion
  is a newer libz3 in the images plus a clean re-adjudication of the 67 + 33 rows.

**Why it fails:** BDDs for large memories blow the 40-bit cap immediately, and mununu's
own array→CHC lowering is where the one soundness-bug candidate lives. External tools carry
mature array theory; mununu's owned array handling is the weakest link — the *only*
vs-ground-truth mismatch in the whole run (1/32 checked) is here. Notably, the inter-engine
contradiction check **missed** it (only SPACER decided the instance), so this category is
also where mununu's soundness *net* is thinnest.

> **Mitigation shipped (`reach_portfolio::collect`, 2026-07-13):** the portfolio now
> refuses a *sole-decider* SPACER `reachable`. SPACER decides from a Horn derivation over
> the (buggy) CHC encoding, whereas every other member exhibits a concrete witness; so an
> uncorroborated spacer-reachable is dropped to a sound `Unknown` instead of a spurious
> `Reachable`. On `vcegar_arrays_itc99_b12_p2` the portfolio therefore now *abstains*
> rather than emitting the wrong verdict. This is a **defensive** guard, not the root-cause
> fix: the btor2→CHC array/BV encoding bug itself is still open, and until it is fixed
> SPACER cannot contribute a `reachable` verdict on its own.

### E. Nonlinear datapath arithmetic (multipliers, `bvurem`/`bvmul`) → BDD + interpolation both choke

- **Example (btormc+Pono find the CEX, owned miss):** `mul7`.
- **Interpolation ceiling (measured elsewhere):** the `gen43`-class needs an interpolant of
  the form `(or (= s32 s17) (= s19 (bvurem s19 s26)))`; cvc5's SyGuS `get-interpolant` search
  took **105 s** to reach that `bvurem` grammar depth — past the per-query budget.

**Why it fails:** BDD representations of multiplication are provably exponential, so the
exact engine cannot build the relation; and mununu's McMillan interpolation engine
(`native_interp`) is only as fast as cvc5's interpolant search, which explodes in
grammar depth on nonlinear operators. Owned engines can decide *linear* datapath properties
but wall on multiplier/modulo relations.

## An important nuance: `gen12` / `gen14` / `gen39`

In a *separate* single-engine eval, mununu's owned **McMillan `native_interp`** uniquely
decides `gen12/14/39` — cases the whole rest of the portfolio (incl. in-process SPACER,
btormc, Pono) left `unknown`. In the *original* FINAL.md portfolio run they showed as
`unreach=[pono]`, because `native_interp` was then a **last-resort** member (it fired only
when the other engines abstained) and Pono decided them first inside the shared 120 s.

> **Change shipped (`reach_portfolio::decide_reach_portfolio_parallel`, 2026-07-13):**
> `native_interp` is now a **first-class parallel member** with an early-cancellation flag,
> so in the default `btor2 verify` path it runs *concurrently* and gets `interp` credit on
> any design it decides — while still bailing the instant a faster engine decides, so it
> costs ~nothing on the common path. Whether that converts any HWMCC instance into an
> *owned-unique* decide (vs. a co-decide with Pono) is what the full-suite re-measurement
> settles; on `gen12/14/39` specifically Pono also decides them, so they are co-decides, not
> owned-unique. This is the one place the owned interpolation engine has a genuine edge on
> this suite.

## The other axis: property class (not exercised by bv-track)

The bv-track is 100 % safety, so it never tests the classes where the owned engines are
strongest *or* weakest by *kind*:

- **Response-liveness** `AG(req → AF grant)` — owned engines handle it only via the
  liveness-to-safety (`l2s`) reduction, then the same safety portfolio; the Category A–E
  limits apply to the reduced product.
- **Branching-time recoverability** `AG EF good` — this is where the owned KMTS 3-valued
  cube is the *only* decider (SVA/LTL and every external bv tool cannot even state it), but
  it is decided on *abstracted* models, and above 40 bits it depends on the cube + ranking
  certificate, not the exact engine.

So a fair one-line summary: **on real bit-vector safety benchmarks, mununu's owned engines
are gated by state-space size (exact engine, Cat A), inductive-invariant synthesis (proofs,
Cat B), BMC throughput (deep CEX, Cat C), array theory (Cat D, + the one soundness bug), and
nonlinear arithmetic (Cat E) — which is why external btormc/Pono carry the bulk. mununu's
differentiated value is elsewhere: branching-time recoverability that no bv tool can state,
and synthesis.**
