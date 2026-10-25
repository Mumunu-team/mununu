# Consumer briefing — `verify-recoverability --pin`: recovery out of reset (2026-10)

> Source of truth: [`apply_pins`](../../crates/mununu-core/src/adapter/recoverability.rs) — surface: (CLI+API)

**Audience:** rosf, monono, mununu-ui and every client of `btor2 verify-recoverability` / `sv verify-recoverability`. Additive: no existing request or verdict changes.

## TL;DR

- **New: `--pin SIGNAL=VALUE`** (repeatable) on `btor2 verify-recoverability` and `sv verify-recoverability`; API `pins: ["SIGNAL=VALUE"]` on `/api/v1/btor2/verify-recoverability` and `/api/v1/sv/verify-recoverability`. The input is tied to the constant before `AG EF target` is decided, so `outcome` and the exit code answer for those values, and the response echoes the applied pins under `pins`.
- **Why it matters.** With the reset free, every state recovers by pulsing it, so the plain verdict reads "recoverable, reset included" and is `holds` on any design whose reset reaches the target. Until now the only way to hold the reset inactive was `--partition rst_n=1`, whose cell sits in the `refinement` while `outcome` and the exit code kept the reset-free answer. Measured on a rod-drive controller with a deliberately absorbing FAULT state: `outcome: holds`, exit 0, and the `rst_n=1` partition cell `violated`. With `--pin rst_n=1` the verdict is `violated`, exit 2.
- **A pin that names no input is an error** (exit 1 on the CLI — the name is checked after the lift — and 400 on the API), so a typo cannot leave the reset free.
- **Unchanged:** every request without `pins`, `--partition` and its refinement, the engines.
- **Schemas:** `btor2-verify-recoverability-request`, `sv-verify-recoverability-request` (optional `pins`), `btor2-verify-recoverability-response` and `sv-verify-recoverability-report` (`pins`, omitted when empty).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | does not call the recoverability verbs | none |
| **monono** | lanes that ask recoverability with a partition to hold the reset can gate on `outcome` with `--pin` instead | optional |
| **mununu-ui** | a recoverability form can offer pins | optional |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes | new flag and API key |
| `mununu/docker/Dockerfile.dev` | yes | the UI's dev backend |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | no | profiling scripts do not pin |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction is unchanged |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | at the next pin | additive |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | at the next pin | additive |

## Verification steps for every consumer

```bash
mununu sv verify-recoverability rod_drive_ctrl.sv --top rod_drive_ctrl --target "state_q == 0"; echo $?
#   "outcome": "holds", exit 0 — the reset escape
mununu sv verify-recoverability rod_drive_ctrl.sv --top rod_drive_ctrl --target "state_q == 0" --pin rst_n=1; echo $?
#   "outcome": "violated", "pins": ["rst_n=1"], exit 2
```

## Provenance

- Found preparing the EmTech tutorial recordings. Test at the seam both surfaces share: `api::handlers::…::a_pinned_reset_decides_recoverability_out_of_reset` (a one-bit trap only the reset clears: free → `holds`, pinned → `violated`, an unknown pin → 400).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- A trace for a violated recoverability on these verbs; `sv verify-auto` with an `// @mununu_guarantee AG EF (…)` annotation returns the trap-certificate trace.
