# Consumer briefing — mununu 0.5.1: `--frontend sv2v` keeps the slang fallback (2026-10)

> Source of truth: `SvLiftRequest::to_sv_lift` in [`crates/mununu-core/src/api/models.rs`](../../crates/mununu-core/src/api/models.rs) and `From<SvFrontendArg>` in [`crates/mununu-cli/src/main.rs`](../../crates/mununu-cli/src/main.rs) — surface: (CLI+API)

**Audience:** everyone migrating to 0.5 this week. **Pin 0.5.1, not 0.5.0.** A fix to the 0.5 clean break ([`2026-10-mununu-0.5-migration.md`](2026-10-mununu-0.5-migration.md)).

## TL;DR

- **The defect (0.5.0).** `--frontend sv2v` (and the API `lift.frontend: "sv2v"`) replaced `--preprocess-sv2v`, but it mapped to a forced `read_verilog`. Before 0.5, `--preprocess-sv2v` kept the `auto` front-end order: sv2v, then `read_verilog`, with **slang as the fallback** when the first attempt fails. sv2v cannot parse a `bind` statement, so on 0.5.0 every design with a bound checker failed to lift under `--frontend sv2v`. rosf's cheatsheet lost its §4 claims to it (`unexpected token 'bind'`).
- **The fix (0.5.1).** `sv2v` now means sv2v first, then the `auto` order with its slang fallback, which is exactly the pre-0.5 `--preprocess-sv2v` behaviour on both surfaces. `--frontend verilog` still forces `read_verilog` alone, and `--frontend slang` still forces slang.
- **No verdict changes** against 0.4: rosf's cheatsheet gives the same 31 of 39 claims on a pre-0.5 build and on 0.5.1. The 8 failing claims are §7 claims that were already stale on 0.4 (instance-path names, decided traps).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | the cheatsheet passes `--frontend sv2v` on bound designs | pin `0.5.1` |
| **monono** | lanes that pass `--frontend sv2v` (formerly `--preprocess-sv2v`) on bound checkers | pin `0.5.1` |
| **mununu-ui** | requests with `lift.frontend: "sv2v"` | none beyond the 0.5.1 backend |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes | 0.5.1 replaces 0.5.0 |
| `mununu/docker/Dockerfile.dev` | yes | the UI's dev backend |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | rosf and monono lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | no | profiling scripts do not use `--frontend sv2v` |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction is unchanged |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | yes, with the 0.5 migration | pin 0.5.1 |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | yes, with the 0.5 migration | pin 0.5.1 |

## Verification steps for every consumer

```bash
mununu --version                                                    # 0.5.1
mununu sv verify-auto rtl/arbiter.sv --source rtl/arbiter_sva.sv --top arbiter --frontend sv2v --pin rst_n=1
#   lifts (the `bind` in arbiter_sva.sv goes through the slang fallback); 0.5.0 failed here
```

## Provenance

- Branch `fix/frontend-sv2v-fallback`. Reproducer: `api::schema::tests::the_sv2v_frontend_keeps_the_auto_fallback` fails on 0.5.0 and passes here (shown with `--features api`; `make verify-repro` builds the lib without features, where the `api` module does not exist). The CLI mapping has its own assertion in `frontend_arg_maps_to_core_variant`. End to end: rosf's cheatsheet run in `mununu-sva` against 0.5.0 (10 of 39 failed), 0.5.1 (8 failed) and a pre-0.5 build `15d87e5` with rosf's unmodified script (the same 8 failed).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md), [`docs/policies/reproducers-before-fixes.md`](../policies/reproducers-before-fixes.md).

## Not covered here

- rosf's stale §7 cheatsheet claims, which rosf fixes on its side.
