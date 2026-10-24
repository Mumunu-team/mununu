# Consumer briefing — 0.5 exit table: usage errors exit 64, `context eval` gates on its verdict (2026-10)

> Source of truth: [`USAGE_EXIT` / `ci_exit_code` / `context_eval_outcome`](../../crates/mununu-cli/src/main.rs) — surface: CLI

**Audience:** rosf, monono, mununu-ui and every script that reads mununu's exit status. First cluster of the 0.5 clean break ([`docs/design/surface-cohesion-0.5.md`](../design/surface-cohesion-0.5.md) §2.1, approved 2026-10-23).

## TL;DR

- **A rejected command line exits 64, not 2.** An unknown flag, a missing argument or a bad value used to exit with clap's default `2`, which is also the VIOLATED code; rosf read a mistyped flag as a violation that way (mununu#541). It now exits `64` (`EX_USAGE`). `--help` and `--version` still exit `0`.
- **`context eval` now gates on its verdict.** It used to exit `0` whatever the formula's value. It now takes `--fail-on` like every verify verb: the formula `holds` iff every initial state satisfies it, and a refuted formula exits **`2`** by default. `--sanity` findings count as `unknown` (exit `3` under `--fail-on unknown`). `--fail-on none` restores the old always-`0` behaviour. The printed text is unchanged.
- **The table, for every verb:** `0` pass · `1` tool error · `2` violated · `3` unknown under `--fail-on unknown` · `4` expectations unmet · `64` usage error.
- **Unchanged:** every other verb's codes; the JSON documents; the HTTP API (no exit codes).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | a usage error is `64`, not `2`; the adapter currently maps `2` to a verdict | map `64` to `skipped` like `1` (the misread of mununu#541 disappears); the cheatsheet's `context eval` calls exit `2` on a refuted formula |
| **monono** | `context eval` exits `2` on a refuted formula, so a `set -e` script that runs it on an expected violation stops there | pass `--fail-on none` where the script reads the verdict from the output itself, or rely on the exit code instead of the `Initial states satisfying:` grep |
| **mununu-ui** | none (HTTP API only) | none |

In this repo, `examples/verify/m1_opentitan_uart_tx/validate.sh` and `m2_opentitan_csrng_main_sm/validate.sh` read the verdict from the output under `set -e` and now pass `--fail-on none`; the `tests/system/*.sh` scripts already tolerate a non-zero exit.

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the exit codes live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | rosf and monono lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not run the CLI gate |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | exit `64` for usage errors |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | `context eval` exits `2` on a refuted formula |

## Verification steps for every consumer

```bash
mununu sv verify-auto --no-such-flag; echo $?                     # 64
mununu context eval m.ctxdsl --automaton A --formula refuted; echo $?        # 2
mununu context eval m.ctxdsl --automaton A --formula refuted --fail-on none; echo $?   # 0
```

## Provenance

- Branch `feat/exit-codes-0.5`; tests `crates/mununu-cli/tests/exit_codes.rs` (the binary's exit status for a usage error, help / version, and `context eval` on a holding and a refuted formula) and `context_eval_fails_the_gate_on_a_refuted_formula_and_flags_sanity_findings`.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The game's `--fault` polarity (E4 of the review) is stated in the verdict envelope, the next 0.5 cluster.
- `context eval --json` arrives with the verdict envelope.
