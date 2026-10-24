# Consumer briefing — 0.5 flags, part (a): `--pin`, `--partition`, `--init-values`, `--controllable`; `verify --strict` is `--fail-on` (2026-10)

> Source of truth: the flag definitions in [`crates/mununu-cli/src/main.rs`](../../crates/mununu-cli/src/main.rs) and the request types in [`crates/mununu-core/src/api/models.rs`](../../crates/mununu-core/src/api/models.rs) — surface: (CLI+API)

**Audience:** rosf, monono, mununu-ui and every script that passes these flags or sends these request keys. Fourth cluster of the 0.5 clean break ([`docs/design/surface-cohesion-0.5.md`](../design/surface-cohesion-0.5.md) §2.5, findings F1, F6 and F8).

## TL;DR

- **One spelling per meaning.** `--config-value` and `--config-values` meant three different things, and the API had one key for all of them. Each meaning now has its own name, with no alias, as the 0.5 clean break decided:

| meaning | verbs | CLI before → after | API key before → after |
|---|---|---|---|
| pin an input to one constant (a scoped verdict) | `sv verify-auto`, `sv mutate` | `--config-value SIG=V` → **`--pin SIG=V`** | `config_values` → **`pins`** |
| split the verdict over enumerated values | `btor2` / `sv verify-recoverability` | `--config-values NAME=v1,v2` → **`--partition NAME=v1,v2`** | `config_values` → **`partitions`** |
| a register's admissible power-up values | `btor2` / `sv cegar` | `--config-values REG=v1,v2` → **`--init-values REG=v1,v2`** | `config_values` → **`init_values`** |
| a controller-owned input | `btor2` / `sv cegar` (game already used it) | `--controllable-input X` → **`--controllable X`** | `controllable_inputs` → **`controllable`** (also on `/context/import`) |

- **The old API keys are refused.** The seven renamed request types (`sv/verify-auto`, `sv/mutate`, `btor2` / `sv verify-recoverability`, `btor2` / `sv cegar`, `context/import`) reject unknown fields with a 400 that names the field. A client still sending `config_values` gets an error, not a run with its pins silently ignored.
- **`verify --strict` is gone; `verify` takes `--fail-on`.** A violated property now exits **2 by default**; it used to exit 0 unless `--strict` was passed. A vacuous conditional verdict counts as `unknown` (exit 3 under `--fail-on unknown`). `--fail-on none` restores the old exit-0 behaviour. Other verbs' `--strict` (warnings as errors in `memory check`, `codesign`, `sv validate`) is a different thing and stays.
- **Unchanged:** values and their syntax, verdicts, and the `diagnostics.config_values` field of the `sv verify-auto` response (it reports the applied pins).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | the adapter passes `--config-value` to `sv verify-auto` and probes it in `--help` | pass `--pin`; update the `--help` probe and `docs/tool-contracts.md`; an old binary lacks `--pin`, so the probe reads the lane as `Incompatible` until the 0.5 pin |
| **monono** | lanes that pass `--config-value` or run `mununu verify` expecting exit 0 on a violation | pass `--pin`; add `--fail-on none` where the lane reads the verdict itself |
| **mununu-ui** | requests that send `config_values` / `controllable_inputs` now get a 400 | rename the keys in `src/api/endpoints.ts` |

In this repo the docs, wiki, tutorials, examples, `scripts/vendor_opentitan_ip.py` (the generated `run.sh`) and every `validate.sh` that runs `mununu verify` under `set -e` (now `--fail-on none`, since they assert the verdicts themselves) follow.

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the flags live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | rosf and monono lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | no | profiling scripts pass none of the renamed flags |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not run these verbs |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | yes, with the adapter change | `--config-value` → `--pin` |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | yes, with the lane changes | `--pin`, `verify` exit codes |

## Verification steps for every consumer

```bash
mununu sv verify-auto top.sv --top top --pin rst_ni=1 --json | jq .diagnostics.config_values
mununu sv verify-auto top.sv --config-value rst_ni=1; echo $?          # 64 (usage error)
mununu verify verify.toml; echo $?                                      # 2 when a property is violated
```

## Provenance

- Branch `feat/flags-pin-partition-0.5`. Tests: `an_sv_verify_auto_request_with_the_pre_0_5_key_is_refused`; the handler suites under all features; `sv_mutate_accepts_the_lift_posture_flags`; the schema drift tests (five schemas regenerated); the legacy `cli_session` suite.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The shared lift flags (`SvLiftArgs` on every `sv` verb), `--frontend sv2v` and the API `lift` / `frontend` keys: part (b).
- `--predicate EXPR` everywhere: part (c). One timeout flag and positive-form API keys: part (d).
