# Consumer briefing — a property's local variables expand over their domain (`ASSERT_FPV_LINEAR_FSM`), the `until` family, property-level `and`, a constant `disable iff`; the bounded-chain monitor judges several weak-until blocks (2026-10)

> Source of truth: [`expand_local_vars`](../../crates/mununu-core/src/adapter/slang/assertion_rewrite.rs) + [`consequent_expr`](../../crates/mununu-core/src/adapter/slang/translate.rs) + [`sites_in_conjunctive_position`](../../crates/mununu-core/src/adapter/btor2/bad_monitor.rs) — surface: (CLI+API) — `sv verify-auto` / `POST /api/v1/sv/verify-auto`

**Audience:** rosf, monono, mununu-ui and anyone whose gate pins `unsupported` counts or verdicts of `sv verify-auto` on OpenTitan RTL. PR B of the 2026-10-22 fault-injection plan.

## TL;DR

- **Four SVA spellings leave `unsupported`.** All exact:
  - **A property with local variables** in the one shape OpenTitan's `ASSERT_FPV_LINEAR_FSM` uses — `(ante, v = E) |-> C(v)`, `v` assigned once in the antecedent's match item and read only in the consequent — becomes one implication per value of `v`'s domain (`E == d` conjoined to the antecedent, `d` substituted in the consequent): the members of `v`'s enum type (rom_ctrl's seven, keymgr's eleven) or `0..2^W` for a packed `logic [W-1:0]`, capped at 64 cases. An enum domain adds the coverage implication `ante |-> (E == d₁ || … || E == dₙ)`, so a HOLDS never rests on the state leaving its encoding. A variable assigned twice, assigned from another local variable, read in the antecedent, or assigned inside a `##` chain antecedent stays `unsupported`, each with its reason.
  - **`p until q` / `p until_with q`** over booleans: the weak readings as `nu Z. ((q || p) && (q || [] Z))` / `nu Z. (p && (q || [] Z))`. **`s_until` / `s_until_with`**: the μ duals (the universal reading; Mixed class, the exact engine only).
  - **`P1 and P2`** in property position: the conjunction.
  - **`disable iff ((0) !== '0)`** — OpenTitan's `ASSERT(name, prop, clk, 0)`: a constant disable adds no guard and records no reset. A constantly TRUE disable is refused ("the property can never be checked"), not proved.
- **The bounded-chain monitor judges several weak-until blocks.** Before, a body with more than one `nu Z. (e && (b || [] Z))` block declined the reach rescue. Now every block at the body's depth in conjunctive position (combined through `&&`, or under `||` against a boolean) gets its own pending latch, and the reach portfolio decides the row. The `ASSERT_FPV_LINEAR_FSM` expansion is exactly that shape. Two blocks under one `||` still decline.
- No JSON shape change. No CLI flag or route change.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `unsupported` rows on these shapes become verdicts; a row ⊥ with `safety-shape-not-reducible` over two weak-until blocks may now decide | rerun; re-pin counts |
| **monono** | same | rerun the gate |
| **mununu-ui** | none | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | translator + monitor live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the gates below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not translate SVA |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | rows leave `unsupported` |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | same |

## Measured (`mununu-sva`)

| run | before | after |
|---|---|---|
| `e2e_opentitan_sva_census` | 171 holds / 11 skipped / 7 violated / 0 ⊥ / **2 unsupported** (`rom_ctrl_fsm` and `keymgr_ctrl` `SecCmCFILinear_A`, local variables) | **173 holds / 11 skipped / 7 violated / 0 ⊥ / 0 unsupported** (1,608 s): both `SecCmCFILinear_A` rows translate (7 and 11 enum cases + coverage) and **HOLD** — rom_ctrl_fsm's block in 7.5 s, keymgr_ctrl's in 186 s; no other row moves |
| `diff_corpus_monotone_verdict_ledger` | PASS | **PASS** (20 s, no flip) |
| `e2e_opentitan_ip_edn_full` | 56 / 11 / 0 / 0 | **56 / 11 / 0 / 0** unchanged (845 s) |

## Verification steps for every consumer

```bash
# rom_ctrl_fsm's linear-FSM assertion: translates (no `unsupported` row) and decides
cd examples/verify/dc_opentitan_rom_ctrl_fsm/source
mununu sv verify-auto rom_ctrl_fsm.sv --top rom_ctrl_fsm --preprocess-sv2v --source … --json | jq '.unsupported, [.properties[] | select(.label == "SecCmCFILinear_A") | .outcome]'
```

## Provenance

- Fix commit: pending merge — branch `feat/sva-local-var-fsm-linearity`.
- Plan: fault-injection posture, linear-FSM properties and the k-fault adversary (2026-10-22), PR B; the captured slang shapes in `crates/mununu-core/src/adapter/slang/testdata/sva_local_var_forms.{sv,ast.json}`.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- Local variables outside the one shape (several assignments, a `##` chain antecedent, a variable read in the antecedent), sequence-position local variables, `until` over sequence operands.
- The vacuity of the linear-FSM rows on models whose `unused_err_o` is constant (PR A1's antecedent-vacuity check).
