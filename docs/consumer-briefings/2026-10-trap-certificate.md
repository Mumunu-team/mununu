# Consumer briefing — an `AG EF` row left ⊥ is refuted when a trap exists (2026-10)

> Source of truth: [`search_trap`](../../crates/mununu-core/src/adapter/trap_certificate.rs) + [`TRAP_CERTIFICATE_DECIDED_BY`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: (CLI+API) — `sv verify-auto` / `POST /api/v1/sv/verify-auto`, the ⊥ re-plan of every `AG EF good` row

**Audience:** rosf, monono, mununu-ui and anyone whose gate pins `unknown` rows or `decided_by` values of `sv verify-auto` recoverability properties. PR B of the 2026-10-21 verdict-flipping levers plan.

## TL;DR

- **A recoverability row the main pass leaves ⊥ can now be VIOLATED.** `AG EF good` (`good` a single `reg == value`) fails iff some reachable state never reaches `good` again. The ⊥ re-plan first looks for a **trap**: a value `v` of `good`'s own register that is reachable from reset and closed under every transition. Both checks are safety questions on the model as given, decided by the reach portfolio: closure is the `|=>` monitor `R == v |=> R == v` (native k-induction proves a genuine trap at depth 1), reachability is the dual of `EF R == v` (native BMC gives the path). A certified trap makes the row **`violated`**, `decided_by: "trap-certificate"`.
- **It only refutes.** It never produces `holds`; a search that certifies nothing changes nothing and the existing recoverability ladder (exact+COI, cube + `smt-hyper-must`) runs as before.
- **Counterexample:** `counterexample.prefix` / `inputs` is the reset → trap path, `cycle` is empty (the trap fixes one register, so no full-state lasso is claimed). The CLI heads it `counterexample (reset -> trap; the trap is closed, the target is never reached again)`.
- **Three new note kinds** (open-string `kind`, no schema change): `trap-certificate` (Info, `items`: `trap: R == v`, `source: sibling:<row>|enum-domain`, `closure: <engines>`, `reach: <engines>`, `depth: <k>`), `trap-certificate-search` (Info, per-candidate `reachable-but-open` / `closed-but-unreachable` / `closure-undecided` / `reach-undecided` / `contradiction`), `trap-certificate-withheld` (ScopeCaveat, the free leaves).
- **Withheld on an over-approximated cone.** When `good`'s register reads an undriven net, a `--cutpoint` or a black-boxed output in its sequential cone, the path into a trap might not exist in the RTL, so no check runs and the row stays as the ladder leaves it.
- **Opt-out:** `--no-rescue` / API `rescue_bottom_recoverability: false` (the existing switch of the recoverability re-plan). No new flag. `[mununu-timing] … lane=trap-certificate` names the lane.
- No JSON shape change.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** (`--profile industrial`) | an `AG EF` row pinned `unknown` may become `violated` with a path; `decided_by` gains the value `trap-certificate` | rerun; a row that moves is a property that was false under the run's reset pin — read the trap in the note before re-pinning |
| **monono** (direct CLI `--json`) | same | rerun the lane at the next pin |
| **mununu-ui** | three note kinds render like `recoverability-rescue` | none required (RTL verb) |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the operator lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the gates below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not run the re-plan |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | `AG EF` rows may move ⊥ → violated |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | same |

## Measured

| run | before | after |
|---|---|---|
| `e2e_opentitan_ip_edn_full` (OpenTitan `edn` + its 15 system properties, shipped defaults, `mununu-sva`) | 56 holds / 8 violated / **3 ⊥** / 0 unsupported in 922 s; the three `AG EF Idle` rows each paid a 60-s rescue slice and stayed ⊥ | **PASS, 56 holds / 11 violated / 0 ⊥ / 0 unsupported in 851 s**; `edn_main_sm_recover_idle` refuted in 1.5 s (trap `state_q == 382`, from the sibling `edn_main_sm_error_is_terminal`), `edn_ack_sm_0_recover_idle` in 1.4 s (`465`, sibling), `edn_ack_sm_7_recover_idle` in 10.5 s (`465`, register domain); each path ends in `Error` |
| `diff_corpus_monotone_verdict_ledger` (the corpus `AG EF` guarantees with pinned true/false verdicts, `mununu-sva`) | PASS | **PASS, 0 flips** (25 s) |
| `wall_class_matrix` (`mununu-sva`; MathSAT absent, so `craig` = WP), new `trap` column | — | **0 oracle contradictions**; `trap` VIOLATED on all six VIOLATED cases (`staller`, `wide_trap`, `trap_uf_w48`, the three reset-pinned RTL FSMs: aes_cipher, csrng, aes_ctr) and ⊥ on every HOLDS / hard case; marginal reach on the fixed set 0 (another lever refutes each of the six) — the recorded justification is the IP-scale reach above, where the exact cone is over the cap |

The rows are false, not hard: each FSM's SEC_CM `Error` state is reachable (a software `ERR_CODE_TEST` write raises `fatal_loc_events`, which every FSM reads as `local_escalate_i`) and terminal, so under `rst_ni=1` the FSM never returns to `Idle`. The example README called them "an engine gap"; it is corrected in this PR.

## Verification steps for every consumer

```bash
# examples/verify/ip_opentitan_edn/source, in mununu-sva: the three AG EF rows read violated / trap-certificate
sh run.sh --json --source mununu_properties.sv \
  | jq '.properties[] | select(.label | test("recover_idle")) | {label, outcome, decided_by}'
```

## Provenance

- Fix commit: pending merge — branch `feat/trap-certificate`.
- Plan: verdict-flipping levers for the SoC rows (2026-10-21), PR B; ledger §3.18.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md); reproducers per [`docs/policies/reproducers-before-fixes.md`](../policies/reproducers-before-fixes.md).

## Not covered here

- A HOLDS-side certificate (an environment strategy σ with `AG (σ ⇒ AF good)`) — deferred: no SoC row needs it, and σ's totality on the concrete reachable set is the open soundness question.
- Compound traps (`T` over more than one register) and a user hint (`@mununu_trap`).
- A relativized certificate under `--assumes` (a relativized `AG EF` row does not reduce, so the operator does not fire on it).
- The `btor2` / `sv verify-recoverability` verbs (they run the ladder directly, not the ⊥ re-plan).
