# Consumer briefing — the ⊥ re-plan reduces bounded response chains; the re-plan runs once per run (2026-10)

> Source of truth: [`reduce_bounded_chain`](../../crates/mununu-core/src/adapter/reach_rescue.rs) + [`chain_is_exact`](../../crates/mununu-core/src/adapter/btor2/bad_monitor.rs) + [`replan_merged`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: (CLI+API) — `sv verify-auto` / `POST /api/v1/sv/verify-auto`

**Audience:** rosf, monono, mununu-ui and anyone gating on `sv verify-auto` `unknown` rows or reading the `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` breadcrumb. Plan 2, PR C (follows #729, #731).

## TL;DR

- **Bounded response chains are decided.** The ⊥ re-plan's reach rescue used to reduce only `AG(P)` and the one-box `A |=> C`; every translator shape with more than one box — `$rose(x) |-> ##[0:k] y` (a disjunct at every level), `a |=> ##n c`, `throughout` chains, `$stable` leaves, and `e throughout b[->1]` (a weak-until block at the bottom of the chain, judged with a pending latch) — was declined in 0 ms and never reached an engine. They now compile to a `bad` monitor (the one the counterexample extractor already used) and the reach portfolio decides them: BMC for violations, k-induction / interpolation / SPACER for proofs. Sound in both directions on the gated shapes (every `||` with at most one box-bearing side; `[] a || [] b` still declines).
- **The ⊥ re-plan runs once per run, on the merged report.** Portfolio members no longer re-plan their own ⊥ rows (new planner-only option field `replan_defer`); the planner re-plans the merged report once, so a row another engine's main pass decides never pays a rescue slice, and a row no engine decides pays it once instead of once per member. No verdict changes. The rescue's provenance notes (`portfolio-rescue`, `safety-rescue-declined`, `bottom-reason`) now appear on the merged report — before, only the base engine's notes survived the merge. A `portfolio-replan` note counts the rows re-planned and decided.
- **The breadcrumb now holds every member's records.** Under the default sequential portfolio the file used to be truncated by each member, so it held only the last member's pass. Readers should take the LAST record per property.
- **The native SMT encoder accepts constants wider than 64 bits.** A BTOR2 `const` / `consth` / `constd` literal with a bit above position 63 used to be refused, which made every in-process reach-portfolio member (native BMC / k-induction, SPACER, interpolation) abstain on the whole design — on OpenTitan `csrng` no ⊥ re-plan ever ran. `ones` wider than 64 bits was silently encoded as 2^64 − 1 (a wrong constant; no shipped verdict is known to rest on it). Designs with wide constants may now decide rows that were ⊥.
- **Residual ⊥ attribution is sharper.** A bounded-chain row the portfolio ran on is no longer classified `safety-shape-not-reducible`; a row whose rescue slice ran out carries `budget-expired`.
- No JSON shape change; no definite verdict flips. Rows move from `unknown` to `holds` / `violated` only.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `--expect NAME=unknown` pins on bounded-response rows may break where the rows now decide — that is the pin doing its job; portfolio runs are shorter | rerun; move pins |
| **monono** | same; a `violated` bounded-response row now carries its trace (the same monitor) | rerun the gate |
| **mununu-ui** | a new Info note kind `portfolio-replan`; rescue notes now present on portfolio reports; the breadcrumb file may contain more than one record per property (take the last) | render like `portfolio`; breadcrumb readers keep the last record |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` | yes | reducer + planner live in the binary |
| `mununu-sva-pono` | yes | same |
| `mununu-ui` | no | JSON unchanged |

## Measured

`mununu-sva`, shipped defaults (`MUNUNU_PROPERTY_BUDGET_MS` 15 min, `MUNUNU_RESCUE_SLICE_MS` 60 s), branch `feat/bounded-chain-rescue` on top of `4010196`:

| run | before (PR B, `4010196`) | after |
|---|---|---|
| OpenTitan `edn`, its own 52 assertions (`run.sh`) | 23 holds / 8 violated / 21 ⊥ / 0 unsupported, 584 s | **39 / 8 / 5 / 0**, 966 s — `EdnDataStableDisable_A` ×8 HOLD (native k-induction, 1–2 s a row), `EdnDataStable_A` ×8 HOLD (the weak-until block, 3–5 s a row), the 5 fault-flag chains reach the portfolio and stay ⊥ as `budget-expired` |
| `edn` + the 15 system properties (`e2e_opentitan_ip_edn_full`) | 35 / 8 / 24 / 0, 1 342 s | **51 / 8 / 8 / 0**, 1 486 s |
| `pattgen` | 14 / 0 / 0 / 0, 29 s | identical |
| `csrng` (cipher black-boxed) | 20 / 0 / 14 / 3, 187 s — every rescue inconclusive in ~100 ms (the encoder refused the design) | **30 / 0 / 4 / 3**, 469 s — the ten control checks the rescue never reached: six FSM / one-hot error→alert chains HOLD (`explicit` cube, whose SMT image the same encoder had refused), the four `CsrngGenbits*KnownO_A` rows HOLD, the four `prim_count` fault-flag chains reach the portfolio and stay ⊥ as `budget-expired` (the same class as edn's five) |
| wall-class matrix | — | identical table, soundness assertion holds, 122 s |
| `e2e_opentitan_sva_census` (134 rows) | PASS | PASS, 84 holds / 11 violated / 31 skipped / 8 unsupported / 0 ⊥, 2 304 s; against main (`4010196`, same image): 2 transitions, both ⊥ → HOLDS (`prim_esc_receiver`'s `prim_count` `ChangeBackward_A` and `CntErrReported_A`, bounded chains the rescue used to decline), 0 definite flips; walls 2 011 s → 2 867 s, almost all of it `keymgr_ctrl` (256 s → 1 092 s: the cube's SMT image now runs on a design the encoder used to refuse in 40 ms and spends its budget with no verdict change — a planner follow-up) |

Oracle: SymbiYosys `abc pdr` proves both `edn` bus-stability families for all eight endpoints (18 s); `smtbmc` to depth 25 finds no violation; a deliberately false control over the same pending latch fails at step 8. For the fault-flag chains `abc pdr` proves `!err_o` in 8 s — the invariant z3 SPACER does not find within the slice.

## Not covered here

- `[] a || [] b` (two box-bearing disjuncts) and chains deeper than 64 boxes stay declined — the one-path monitor is not exact there.
- `AG EF` recoverability rows and datapath rows behind a black box are unchanged.
