# Consumer briefing — covers under an input-reading `assume` are no longer reported unreachable (2026-10)

> Source of truth: [`ExactSession::attempt`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) (`first_modalities_existential`) — surface: (CLI+API) — `sv verify-auto` / `POST /api/v1/sv/verify-auto`, and every verb that reaches the exact engine

**Audience:** rosf, monono, mununu-ui and anyone who reads `cover` rows (or any `EF …` property) from `sv verify-auto`. Fixes mununu#757.

## TL;DR

- **Verdict change (bugfix).** With any applied `assume` that reads an input, every `cover` came back `violated` with `counterexample.unreachable_target` ("the design never reaches `mununu_assume_ok ∧ <target>`"), decided by `exact-symbolic`, although the target was reachable on an assumption-respecting trace. Those rows now come back `holds`.
- **Cause.** The inputs-in-the-state lift (mununu#656) samples the cycle-0 input into the initial state, and the exact engine required the property at every initial state, so it quantified that input universally. A property that asks for SOME trace (a `cover`, an `EF …`) now reads it existentially: the environment chooses the cycle-0 input. Universal properties (asserts, `AG …`, recoverability `AG EF …`) and the two-player game verbs keep the universal reading.
- No flag, field or schema changes.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `cover` rows under input-reading assumes flip `violated` → `holds` | none; a pinned `--expect-violated` on such a cover now fails, which is correct |
| **monono** | same | re-run the lanes that carry covers with assumes; drop any `--assumes ignore` workaround |
| **mununu-ui** | none (RTL verb) | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the fix lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | covers run here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | cover verdicts |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | same |

## Measured

| run | result |
|---|---|
| `an_existential_property_reads_the_sampled_cycle0_input_existentially` (`adapter/btor2/input_state.rs`) | `EF ok` on `ok = prev ∧ a`: violated on the parent, holds here (`make verify-repro` CONFIRMED against `8b98949`); `AG ok` stays violated |
| EmTech `cmd_ctrl` reproducer (`m_legal_addr` assume + 3 covers) | parent: 3/3 covers `violated`; here: 3/3 `holds` (`exact-symbolic`) |
| EmTech `cmd_ctrl` full SVA (5 assumes, 16 asserts, 5 covers, default `--assumes precondition`) | 16 asserts `holds` (unchanged), 5 covers `holds` (previously only under `--assumes ignore`) |
| `e2e_opentitan_sva_census` | 173 holds / 11 skipped / 7 violated / 0 unknown — identical to the baseline (the census carries no `cover` rows; the 7 violated rows are assertions) |

## Verification steps for every consumer

```bash
mununu sv verify-auto dut.sv --source dut_sva.sv --top dut --frontend slang --json \
  | jq '.properties[] | select(.kind=="cover") | {label, outcome, decided_by}'
```

## Provenance

- Fix commit: pending merge — branch `fix/cover-under-assume`.
- Issue: mununu#757 (found while preparing the EmTech tutorial material).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- A plain state-predicate property with no modality (`mununu_assume_ok == 1` evaluated at reset) keeps the universal reading of the cycle-0 input.
- Formulas whose first modalities disagree (`EF a ∧ AG b`) keep the universal reading.
