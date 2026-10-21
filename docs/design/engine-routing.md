# Engine routing — which engine decides which property, and why

> Status: reference. The map from a verification request to the engine that actually
> decides it, plus the soundness class each engine covers. Written for the agent/CI
> differentiation program (P0.1) so investment targets the *defensible* paths.

mununu has **two distinct portfolios** and one exact oracle. Getting a property to the
right one is the whole game: safety at scale wants the word-level reachability engines;
branching (νμ) properties can only go through the 3-valued engines.

## The engines

| Engine | Decides | Soundness | Scale ceiling |
|---|---|---|---|
| **exact-symbolic** (BDD bit-blast) | full modal-μ incl. νμ, at the *initial state* | definite (exact, no abstraction) | cone ≤ 192 bits by auto-cap (40 by default; 1024 on the recoverability cap ladder); within it the ceiling is the cone's *shape* — a multiplier leaves the exact engine at 9 bits per operand under the default arena (the cube ladder decides it to 32), FSM + timer control logic decides at every width to the 1024-bit ceiling ([measured frontier](../verifying-rtl.md#what-decides-at-what-width--the-measured-frontier)) |
| **cube CEGAR** (+ `smt-hyper-must`) | full modal-μ incl. νμ, over a **predicate abstraction** | Bruns–Godefroid 3-valued (definite verdicts transfer; ⊥ ⇒ refine) | predicate-count bound; needs the right predicates |
| **reach portfolio** (exact ⊕ native BMC/k-induction ⊕ SPACER ⊕ interpolation ⊕ btormc ⊕ Pono ⊕ rIC3, all concurrent) | **`bad`-reachability only** (safety) | each member sound; first-definite wins; disagreement ⇒ contradiction alarm; a member's refusal is carried (mununu#734); subprocess members read a constant-hoisted emission (mununu#736); **SPACER corroborates, never decides alone** in either direction (mununu#740 — libz3 4.13.3 proved a violated HWMCC design safe) and runs only as the isolated, cancellable child | word-level, **no bit cap** (native/SPACER/interp/btormc/Pono/rIC3); rIC3 `-e ic3` is the bit-level IC3 that proves inductive-invariant safety (2026-10-19) |

> Source of truth: [`exact_symbolic_verdict`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs#L2095) · [`cegar_refine_loop`](../../crates/mununu-core/src/adapter/btor2/cegar.rs) · [`decide_reach_portfolio_cancellable`](../../crates/mununu-core/src/adapter/reach_portfolio.rs#L523) — surface: (CLI+API+UI)

**Cube termination bound (`MUNUNU_CUBE_SMT_RLIMIT`).** The exact engine has deterministic
node/iteration budgets; the cube's `smt-hyper-must` must-edge inference can otherwise grind
indefinitely on a **wide combinational cone** (a control signal whose freed-input fanin is
100–250 bits — e.g. i2c's 196-bit cone, a ~2-hour hang). Setting `MUNUNU_CUBE_SMT_RLIMIT` to a
positive integer applies a Z3 `rlimit` (a **machine-independent** resource budget) to each
must-edge query: a query that exceeds it returns `Unknown`, which the inference reads as *no
must-edge* — a **weaker must relation, hence a more conservative (more ⊥) but always SOUND**
abstraction. This turns the hang into a fast, deterministic ⊥ (i2c: ~2 h → 27 s). Unset =
no bound (historical behaviour).

> Source of truth: [`cube_smt_rlimit`](../../crates/mununu-core/src/adapter/btor2/smt_must_edge.rs#L85) — surface: (CLI+API+UI)

## Per-surface routing

| Surface / property | Path → engine | Predicates? |
|---|---|---|
| `btor2/sv verify` (safety) | `decide_reach_portfolio` → **reach portfolio** | none |
| `btor2/sv verify-liveness` (`AG(a→AF b)`) | Biere l2s → `bad`-monitor → **reach portfolio** | none |
| `btor2/sv verify-recoverability` (`AG EF good`) | `exact_symbolic_verdict` → **exact-symbolic** | **none** |
| `btor2 check-fsm` (auto illegal-encoding per FSM reg) | `fsm_encoding_scan` → **reach portfolio** per register | **none** (legal set auto-derived from the design) |
| `sv verify-auto` (default `portfolio-sequential`) | **exact-symbolic → symbolic → explicit**, early-exit | exact leg none; cube legs **auto-seeded** |
| `sv verify-auto` safety ⊥ | escalate → reduce → **reach portfolio** | none |
| `sv verify-auto` cube-**SKIPPED** (atom-less modal, e.g. no-deadlock `nu X.(<> true && [] X)`) | escalate → **exact-symbolic** rescue | none (exact needs no seeding) |
| `btor2 cegar` (explicit) | `cegar_refine_loop` → **cube** | hand-`--predicate` unless auto-seeded upstream |

> Source of truth: [`engine_selection`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs#L2024) + [`verify_auto_portfolio`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs#L1469) (the 3-engine sequential/parallel portfolio) — surface: CLI+API.

**The cube-SKIPPED → exact rescue.** A pinned cube engine (`--engine explicit`/`symbolic`) *skips* a property whose formula it cannot seed — an **atom-less** modal obligation like no-deadlock `nu X.(<> true && [] X)` has no state-cell/combinational atom to build a cube dimension from. The full-state exact engine needs no seeding, so after the cube pass every still-`Skipped` property gets one exact-symbolic attempt and is upgraded in place on a **definite** verdict. It runs only under reset-gating (the exact engine's A.6 precondition) and only on a cube run (the exact main path already ran exact). Sound: the exact engine is the differential oracle, so the upgrade is pure precision, never a contradiction. (This closes the i2c_slave-class gap where the deadlock-freedom obligation was reported `Skipped` under `--engine explicit`.)

> Source of truth: [`rescue_skipped_via_exact`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs#L6161) — surface: (CLI+API+UI)

**The predicate-avoidance rule (P0.2), confirmed by the 2026-07-08 csrng spike:**

1. `verify-auto`'s default (`portfolio-sequential`) tries **exact-symbolic first** — which needs **no predicates** and, on a real OpenTitan FSM, decides recoverability directly (the FSM cone fits under the auto-cap).
2. Only when exact-symbolic is **over-cap** does it fall to the cube legs, which **auto-seed** predicates from the formula's atoms (`seed_from_formula`) — still no hand-written predicates.
3. Hand-written predicates / `config_values` / `counter_bounds` are the **residual** — needed only for the cube's ⊥ ceiling (combinational-input FSM control signals whose cone explodes). That residual is the target of P0.2's follow-up (auto-derive the missing concretizations).

So: **hand-written predicates are already the exception, not the rule** — the default path is auto.

## Defensibility (which paths to lead with)

- **`AG EF` recoverability via exact-symbolic / cube+hyper-must — DEFENSIBLE as a *named* property.** A branching νμ property SVA/LTL cannot express, decided soundly, on real RTL, with no predicates at FSM scale. No mainstream tool offers it. Keep the `verify-recoverability` verb (the user names a meaningful target). **Do not auto-scan it**: reset-free recoverability-to-the-reset-value is *tautological* on a reset-carrying design (reset always provides an escape → `holds` everywhere), and reset-held flags every intended reset-recoverable error — neither is a zero-touch bug signal (soundness finding, 2026-07-08).
- **Illegal-encoding reachability via the reach portfolio — the zero-touch auto bug-finder (P1).** The first P1 slice ships `btor2 check-fsm` (`fsm_encoding_scan`): discover FSM registers, derive each one's legal encoding set from the design (compared constants + reset value), and check from the reset state whether any illegal encoding is reachable. Reset cannot paper it over and design intent cannot excuse it — a reachable out-of-enum value is an unambiguous bug. A *safety* property, so it rides the word-level portfolio (no bit cap). Validated on the real csrng `state_q` (8 legal sparse encodings auto-derived, `holds`).
- **Response-liveness via l2s — partly defensible.** Sound concrete reduction, but liveness-to-safety is known art; keep, don't lead.
- **Safety via the reach portfolio — TABLE-STAKES.** SymbiYosys/commercial do BMC/PDR safety at scale, often better. Keep for completeness + the ⊥ escalation; don't position as the differentiator.

The investment thesis that falls out: **branching properties on the FSM cone** (where mununu is unique *and* under the cap *and* predicate-free) is the defensible core — exactly the P1 auto-property-synthesis + bug-finder target.
