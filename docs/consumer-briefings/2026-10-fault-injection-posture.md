# Consumer briefing — the fault-injection posture: `--fault` / `--fault-model sec-cm` on the SV verbs, `btor2 verify --fault`, `btor2 inject-fault`, the antecedent-vacuity check (2026-10)

> Source of truth: [`inject_faults`](../../crates/mununu-core/src/adapter/btor2/fault.rs) + [`apply_fault_posture`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: (CLI+API) — `sv verify-auto` / `sv mutate` / `btor2 verify` and `POST /api/v1/sv/verify-auto`, `/sv/mutate`, `/btor2/verify`; `btor2 inject-fault` CLI-only

**Audience:** rosf, monono, mununu-ui and anyone who pins `sv verify-auto` JSON, runs OpenTitan's security-countermeasure checks, or reads `vacuity.kind`. PR A1 of the 2026-10-22 fault-injection plan.

## TL;DR

- **Nothing changes unless you opt in.** Without `--fault` / `--fault-model` (API `faults` / `fault_model`) every verdict, note and field value is as before; the two new diagnostics fields are `null` / empty.
- **A new posture.** `--fault REG[:k]` (repeatable; one global budget, default 1; `REG:unbounded`) or `--fault-model sec-cm[:k]` lets an adversary overwrite registers — a free value at a free cycle after `--fault-warmup` (default 10) — as a BTOR2 rewrite with a structural budget, so every engine sees one total model. `sec-cm` finds the sites from the design's own `$rose(X.err_o) |-> ##[0:k] alert` chains (the cells the flag compares); `sec-cm:0` selects without injecting and runs the vacuity check only.
- **Reading a verdict under the posture.** A safety / response HOLDS transfers ("the countermeasure responds for every corruption at these sites within the budget"). A VIOLATED is a real escape of the declared adversary; the trace names the injections (`mununu_fault_en_i` / `mununu_fault_val_i`). An alternating (νμ) row whose cone holds a site is **skipped** with a reason. For OpenTitan alert senders pin the receiver: `--config-value alert_rx_i=85`.
- **A new `vacuity.kind`: `antecedent-unreachable`.** Under any posture, a countermeasure chain row that HOLDS while its flag can never rise (the reach portfolio proves `AG !rose`) carries it, plus a `vacuous-guarantee` note; a reachable antecedent adds an Info `antecedent-reachable` note.
- **JSON additions** (all additive): `diagnostics.fault_model: string | null`, `diagnostics.fault_sites: [{register, width, bound, selected_by, rows}]`; request fields `faults`, `fault_model`, `fault_warmup` on `/sv/verify-auto` and `/sv/mutate`, `faults` / `fault_warmup` on `/btor2/verify`; `btor2 verify --json` gains `fault: string | null`. Four schemas regenerated.
- **New CLI surface:** `btor2 inject-fault FILE --fault REG[:k]… [-o PATH]` prints the rewritten BTOR2 (CLI-only — a file transform for reproducing a verdict with another engine).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** (`--profile industrial`, OpenTitan lanes) | nothing by default; to make the sec_cm rows meaningful add `--fault-model sec-cm --config-value alert_rx_i=85` to the lane and gate on `vacuity.kind != "antecedent-unreachable"` | opt in per lane; the new `kind` value renders like the others |
| **monono** (direct CLI `--json`) | the two new diagnostics fields; the posture is opt-in | none required |
| **mununu-ui** | two nullable/empty diagnostics fields, one more `vacuity.kind` value | none required (RTL verb) |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the rewrite and the flags live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the gates below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | new opt-in flags, additive JSON |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | same |

## Measured (`mununu-sva`)

| run | before | after |
|---|---|---|
| `e2e_opentitan_ip_edn_sec_cm_posture` (new): `sec-cm:0` instrument | — (the 15 rows held; nothing said they were vacuous) | 21 sites (10 counter copies, 9 FSM state cells via the flag mint, 2 register-block cells); **15 of 15 rows HOLD and are `antecedent-unreachable`** — no fault flag can rise on the un-faulted model (817 s) |
| the same, `sec-cm` with one injection, `alert_rx_i=85` | — | **0 violated**; 10 HOLD, 6 of them with an `antecedent-reachable` note (the response is exercised); 5 ⊥ within the budget; the register-write one-hot row stays vacuous — its sites cannot raise its flag (a known site-selection limit) (2,754 s) |
| `e2e_opentitan_ip_edn_full` | 56 / 11 / 0 / 0 | **56 / 11 / 0 / 0** unchanged — the posture is opt-in |
| `diff_corpus_monotone_verdict_ledger` | PASS | **PASS** |

## Verification steps for every consumer

```bash
# the instrument: which countermeasure rows hold with an antecedent the model cannot raise
mununu sv verify-auto edn.sv --top edn … --fault-model sec-cm:0 --json \
  | jq '[.properties[] | select(.vacuity.kind == "antecedent-unreachable") | .label]'
# the posture: the same rows with the adversary in the model
mununu sv verify-auto edn.sv --top edn … --fault-model sec-cm --config-value alert_rx_i=85 --json \
  | jq '{sites: .diagnostics.fault_sites | length, rows: [.properties[] | select(.label | startswith("FpvSecCm")) | {label, outcome, v: .vacuity.kind}]}'
```

## Provenance

- Fix commit: pending merge — branch `feat/fault-injection-posture`.
- Plan: fault-injection posture, linear-FSM properties and the k-fault adversary (2026-10-22), PR A1.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The k-fault adversary verbs (`btor2 verify --fault --target`, `btor2 game --fault`): PR C.
- Minting the sparse-FSM flop's `unused_err_o` from the enum domain on the un-faulted lift (the posture corrupts the state cell directly instead).
- An environment model of the alert receiver (the `alert_rx_i=85` pin is the documented recipe).
