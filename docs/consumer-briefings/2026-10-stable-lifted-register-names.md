# Consumer briefing — lifted register names no longer embed yosys's per-run staging directory (mununu#806, 2026-10)

> Source of truth: [`run_yosys`](../../crates/mununu-core/src/adapter/yosys/mod.rs) and the sv2v branch of `translate_sv` in the same file — surface: (CLI+API) — every SystemVerilog lift that goes through sv2v (`sv verify-auto`, `sv verify`, `sv mutate`, `sv game`, `context import --format sv-yosys`)

**Audience:** rosf, monono and anyone who diffs mununu reports across runs or keys a sidecar, a `--config-value` or a predicate on a lifted register name.

## TL;DR

- **Name change, three edn registers today, any `$func$` temporary in general.** yosys names a function-call temporary after the SOURCE PATH it read, and the sv2v branch of the lift handed it the staged file by its absolute path under the per-run `/tmp/mununu-yosys-<pid>-…/`. Three edn state registers were therefore reported as `u_edn_core.gen_ep_blk$N.u_edn_ack_sm_ep.u_state_regs.sv2v_cast_F2F2E$func$/tmp/mununu-yosys-7-1-0-562696519/preprocessed.sv:3983$4411.$result`, different on every run, in every `trace` step and in `diagnostics`.
- **Now:** yosys runs inside the staging directory and reads `preprocessed.sv` by its bare name, so the same register is `…sv2v_cast_F2F2E$func$preprocessed.sv:3983$4411.$result` on every run (yosys's `$4411` autoidx is deterministic for the same input). Every other path in the yosys script stays absolute; a RELATIVE `--include-dir` is absolutized before the cwd moves.
- **Verdicts are unaffected.** A consumer that stored one of the old names (a sidecar, a config value, a predicate atom) must update it — the old name never matched a second run anyway.
- The slang frontend (`MUNUNU_YOSYS_FRONTEND=slang`) reads the user's own source files, so its `$func$` names carry the user's source path — stable on one machine, not changed here.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | the three edn names (and any other `$func$`-named register) are stable across runs; staleness diffs stop reporting them | drop any special-casing of `/tmp/mununu-yosys` in name comparisons |
| **monono** | same | none required |
| **mununu-ui** | none (RTL verbs) | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the lift lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the gate below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not lift SV through yosys |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | — |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | — |

## Measured

| run | result |
|---|---|
| `e2e_lifted_register_names_carry_no_per_run_temp_path` (new; the reproducer: edn lifted twice, state names identical and free of the temp path) | **passes** on the branch (host, sv2v path: 184 named state registers, identical across the two lifts, none with the `mununu-yosys-` staging prefix; 16 s) |
| the same test on the parent (`main`) in the image | **fails** on `main` `fcd4efe` (host, the test copied into a detached worktree: the two lifts disagree on their state names — the three `$func$` names carry the staging path) |
| verdict-identity gate: `e2e_opentitan_sva_census` vs `main` and `diff_corpus_monotone_verdict_ledger` | **0 verdict moves** — 191 rows, 173 holds / 7 violated / 11 skipped / 0 ⊥ on both (2,144 s); ledger PASS (38 s) |
| `e2e_opentitan_ip_edn_full` (pins unchanged) | pins unchanged — 67 properties, 11 violated as on `main` (1,053 s) |

## Provenance

- Issue: mununu#806 (class `ergonomics`, `found:internal`).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- `$paramod`-style names that carry file positions from the user's own sources.
- The slang frontend's `$func$` names (the user's source path, not a temp path).
