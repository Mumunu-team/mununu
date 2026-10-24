# Consumer briefing — 0.5 flags, part (b): one lift-flag set on every `sv` verb, `--frontend sv2v`, the API `lift` object (2026-10)

> Source of truth: [`SvLiftRequest`](../../crates/mununu-core/src/api/models.rs) and the CLI's `SvLiftArgs` in [`crates/mununu-cli/src/main.rs`](../../crates/mununu-cli/src/main.rs) — surface: (CLI+API)

**Audience:** rosf, monono, mununu-ui and every client of the `sv` verbs. Fifth cluster of the 0.5 clean break ([`docs/design/surface-cohesion-0.5.md`](../design/surface-cohesion-0.5.md) §2.5, findings F3 and F4).

## TL;DR

- **CLI: one lift-flag set.** `sv verify-auto` used its own copy of the lift flags. It now shares `SvLiftArgs` with every other `sv` verb, so it gains `--design-dir`, `--search-path` and `--exclude`. Every `sv` verb gains `--param`, `--lift-override` and `--cutpoint`, which only `verify-auto` and `mutate` had. The flags and their syntax are unchanged where they existed.
- **CLI: `--preprocess-sv2v` is gone; `--frontend sv2v` replaces it.** `--frontend` takes `auto`, `verilog`, `slang` or `sv2v` (sv2v, then `read_verilog`). This applies to every `sv` verb, including `discover`, `emit-btor2-per-module` and `cegar`. A leftover `--preprocess-sv2v` is a usage error (exit 64).
- **API: the design is one `lift` object.** Every `sv` route takes `lift: { source, source_name?, additional_sources?, top?, frontend?, params?, lift_overrides?, cutpoints?, merge_clock_domains? }` instead of top-level `source` / `additional_sources` / `top` / `use_sv2v` / `use_slang` / `merge_clock_domains` / `params` / `lift_overrides` / `cutpoint`. The routes are `sv/verify`, `sv/verify-liveness`, `-liveness-all`, `sv/verify-recoverability`, `sv/check-fsm`, `sv/lint`, `sv/cdc-lint`, `sv/mutate`, `sv/cegar` and `sv/verify-auto`. `frontend` is a string with the CLI's four values; the API could not ask for `verilog` before. The `lift` object and the ten request types refuse unknown keys, so an old flat request is a 400 that names the field. `cutpoint` (singular) is now `cutpoints`.
- **Every verb honours the whole lift posture.** `sv verify`, `-liveness`, `-recoverability`, `check-fsm`, `lint`, `cdc-lint` and `cegar` used to ignore params, cutpoints and lift overrides. They now apply them through one core `SvLift::staged()`.
- **Unchanged:** verdicts, response shapes, and `sv verify-auto`'s rule that lift overrides replace sources for the lift only (the SVA is still extracted from the originals). The `verify.toml` key `use_sv2v` is a project-config field and stays.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | the adapter passes `--frontend slang` (unchanged); it passes `--param` / `--lift-override` / `--cutpoint`, unchanged in spelling | rerun the `--help` probe; nothing else, unless a lane passes `--preprocess-sv2v` (→ `--frontend sv2v`) |
| **monono** | lanes that pass `--preprocess-sv2v` | `--frontend sv2v` |
| **mununu-ui** | the extraction tab's `sv/cegar` request (and any other `sv` route) sends the flat shape | nest the design under `lift`, and `use_sv2v: true` → `lift.frontend: "sv2v"` |

In this repo the docs, wiki, examples and validators that passed `--preprocess-sv2v` or sent the flat API shape follow, as do the verify-auto notes that suggested `--frontend verilog --preprocess-sv2v` (now `--frontend sv2v`).

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the flags and the request shape live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | rosf and monono lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | no | profiling scripts pass none of the removed flags |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not run the `sv` verbs |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | probe only |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | yes, with the lane change | `--frontend sv2v` |

## Verification steps for every consumer

```bash
mununu sv verify-auto --design-dir rtl/ --top top --param DEPTH=4 --frontend slang --json | jq .diagnostics.frontend
curl -s localhost:8080/api/v1/sv/check-fsm -H 'Content-Type: application/json' \
  -d '{"lift": {"source": "module m(input logic clk); endmodule", "frontend": "sv2v"}}'
```

## Provenance

- Branch `feat/flags-lift-args-0.5`. Tests: the handler suites under all features (literal and JSON-body requests on the nested shape), the `sv_verify` suite, the CLI parse tests (`SvLiftArgs` with the new fields), the schema drift tests (nine request schemas regenerated; `sv/cegar` is not schema-pinned).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- `--predicate EXPR` everywhere (part c); one timeout flag and positive-form API keys such as `gate_reset` / `rescue` (part d).
