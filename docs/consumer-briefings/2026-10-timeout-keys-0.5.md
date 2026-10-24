# Consumer briefing — 0.5 flags, part (d): one `--timeout-ms`, positive-form API keys, the no-op shadow switch removed (2026-10)

> Source of truth: the `btor2 verify` / `verify-justice` flags in [`crates/mununu-cli/src/main.rs`](../../crates/mununu-cli/src/main.rs) and `SvVerifyAutoRequest` in [`crates/mununu-core/src/api/models.rs`](../../crates/mununu-core/src/api/models.rs) — surface: (CLI+API)

**Audience:** rosf, monono, mununu-ui and every script that sets an engine budget or the verify-auto engine switches. Seventh and last flag cluster of the 0.5 clean break ([`docs/design/surface-cohesion-0.5.md`](../design/surface-cohesion-0.5.md) §2.5, findings F7 and F9).

## TL;DR

- **One wall-clock flag, `--timeout-ms`.**
  - `btor2 verify --owned-timeout-ms` is gone. `--timeout-ms` is the per-engine budget for whichever members run: the subprocess members of the full portfolio, or every engine under `--owned-only` (default 60000 in both cases).
  - `btor2 verify-justice --budget-ms` is now `--timeout-ms`, and the API `/btor2/verify-justice` key `budget_ms` is now `timeout_ms`.
  - Engine budgets that change verdict determinism stay environment variables (`MUNUNU_BDD_TIME_BUDGET_MS`, …).
- **Positive-form API keys on `/api/v1/sv/verify-auto`.** The CLI flags are unchanged.

  | before | after | default |
  |---|---|---|
  | `rescue_bottom_safety`, `rescue_bottom_liveness`, `rescue_bottom_recoverability` | `rescue` (one switch, as the CLI's `--no-rescue`) | `true` |
  | `no_antecedent_propagate: true` | `antecedent_propagate: false` | `true` |
  | `no_input_state: true` | `input_state: false` | `true` |

  The request refuses unknown keys (since part a), so a pre-0.5 key is a 400 that names it.
- **The no-op shadow switch is removed.** `--no-antecedent-shadow` (`sv verify-auto` and the hidden engine child verb) and the API key `no_antecedent_shadow` had done nothing since mununu#677, and are gone. Passing them is a usage error (64) or a 400. `MUNUNU_NO_ANTECEDENT_SHADOW` was already read by nothing.
- **Unchanged:** verdicts and response shapes.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | the adapter passes none of these | none |
| **monono** | lanes that pass `--owned-timeout-ms`, `--budget-ms` or `--no-antecedent-shadow` | `--timeout-ms`; drop `--no-antecedent-shadow` |
| **mununu-ui** | requests that send `rescue_bottom_*`, `no_input_state` or `no_antecedent_*` | the positive keys above |

In this repo `scripts/profile_cases.py` (`--timeout-ms`), an e2e test comment, `docs/verifying-rtl.md`, `docs/env-vars.md` and the CI wiki page follow.

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the flags and keys live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | rosf and monono lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | yes, for `scripts/profile_cases.py` | it now passes `--timeout-ms` |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not run these verbs |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | not used by rosf |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | yes, with the lane changes | flag renames |

## Verification steps for every consumer

```bash
mununu btor2 verify d.btor2 --owned-only --timeout-ms 120000 | jq -r .outcome
mununu btor2 verify d.btor2 --owned-timeout-ms 1; echo $?        # 64
```

## Provenance

- Branch `feat/flags-timeout-keys-0.5`. Tests: the handler suites under all features, the `engine_isolate` suite (its child no longer receives the removed flag), the CLI tests, and the schema drift tests (`sv-verify-auto-request` regenerated).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The unified `Trace` type, the verb merges and the route renames with schemas are the remaining 0.5 clusters.
