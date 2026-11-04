# Consumer briefing — the SystemVerilog lift is yosys-slang only: `--frontend`, `lift.frontend`, `use_sv2v`, `--preprocessor` and `sv preprocess` are gone (2026-11)

> Source of truth: [`SvFrontend`](../../crates/mununu-core/src/adapter/yosys/mod.rs) + [`plan_lift`](../../crates/mununu-core/src/adapter/yosys/mod.rs) + [`read_slang_cmd`](../../crates/mununu-core/src/adapter/yosys/mod.rs) — surface: (CLI+API+UI) — every path that lifts SystemVerilog to BTOR2.

**Audience:** rosf, monono, mununu-ui, every client of the `sv` verbs or of `context … --adapter sv-yosys`, and every `verify.toml` project with SystemVerilog sources. Step N1 of the commercial-gap plan (2026-11-03): one product front end. Supersedes the line "the `verify.toml` key `use_sv2v` is a project-config field and stays" of [`2026-10-lift-args-0.5.md`](2026-10-lift-args-0.5.md).

## TL;DR

- **One front end.** Every SystemVerilog lift runs yosys-slang's `read_slang`, built into yosys ≥ 0.67 (the pinned `mununu-sva` image has 0.68). `read_verilog`, sv2v, the `auto` order (read_verilog + sv2v first, slang on error), the Verific check and the `preprocessed.sv` staging are out of the product. `read_verilog` + sv2v survives only as the differential oracle of `test_support` builds (`cargo test`), unreachable from the CLI and the API.
- **CLI, hard break (usage error, exit 64).** `--frontend` is removed from every `sv` verb (`verify`, `verify-liveness`, `verify-liveness-all`, `verify-recoverability`, `check-fsm`, `lint`, `cdc-lint`, `mutate`, `cegar`, `verify-auto`, `discover`, `emit-btor2-per-module`); `--preprocessor` is removed from `context eval`, `context summarize`, `context predicates` and `context synth`; the `sv preprocess` subcommand is removed. The `--help` text of every `sv` verb no longer mentions a front end.
- **API.** `lift.frontend` is removed from the ten `sv` route request types; `SvLiftRequest` refuses unknown keys, so a request that still sends it is a 400 naming the field. `use_sv2v` is removed from `POST /api/v1/context/import` (`ContextImportRequest` also refuses unknown keys). In the `sv/verify-auto` response, `diagnostics.frontend_fallback_reason` is removed; `diagnostics.frontend` stays and always reads `"yosys-slang (read_slang)"`.
- **Notes and remedies.** The `lift-frontend` note is no longer emitted (there is nothing to report: one front end, no fallback). The partial-write refusals (`q[hi:lo] <= …` on the exact engine and on `verify-auto`) no longer recommend the sv2v path; they say "writing the register whole (`q <= …` rather than `q[hi:lo] <= …`) is the route that works".
- **`verify.toml`.** `[[sources]] options.use_sv2v` is refused at validation: `[[sources]] `<id>`: options.use_sv2v is no longer supported — the SystemVerilog lift runs yosys-slang's `read_slang` only; remove the key`.
- **Environment.** `MUNUNU_YOSYS_FRONTEND` is gone (ignored). `MUNUNU_YOSYS_SLANG_PLUGIN` is now the only way to load a yosys-slang plugin (`yosys -m <plugin>`); unset, the built-in `read_slang` is used, and a yosys without it fails the lift with an error that names the fix (before, the plugin was searched at fixed paths on every lift). `MUNUNU_SV2V_PATH` is read by `test_support` builds only.
- **Error text.** A failed lift reads `SV → BTOR2 (yosys-slang): …` (was `SV → BTOR2 (read_verilog + sv2v): …` or `(yosys-slang)` per attempt, with a `fallback_reason` after the second). A lift on a yosys without `read_slang` appends the hint to install one or set the plugin variable.
- **Verdicts.** Identical wherever the `auto` order already ended on slang: every design with `assert property` / `bind`, packages, interfaces, module-header imports — every OpenTitan block. A design that lifted under `read_verilog` + sv2v and never fell back (Verilog-2005, SV that sv2v could rewrite) now lifts under slang; its BTOR2 is elaborated by a different front end, and the register names are the `instance.signal` names slang gives, not sv2v's mangled ones. Immediate `assert` statements inside procedural blocks are not lifted into `bad` nodes under either front end: sv2v dropped them, and `read_slang --ignore-assertions` drops them (without the flag yosys-slang refuses any temporal property — `|=>`, `##1`, `$rose`, `s_eventually` — as "unsupported SVA feature", a hard elaboration error, and has no finer switch; measured 2026-11-04 on yosys 0.68+120). Nothing changes there.
- **Lift cache** (`MUNUNU_LIFT_CACHE_DIR`, set by `make e2e*` only): the manifest format is 2 (one attempt per entry); entries written by format 1 are misses, never errors.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `crates/rosf-core/src/verify/mununu.rs` pushes `--frontend slang` on every `sv verify-auto` call (L595) and probes the flag in `--help` (L91–93): with the new binary the probe fails and a call without the probe is a usage error, exit 64. `diagnostics.frontend_fallback_reason` (L285, L933, the `a_frontend_fallback_is_never_silent` test) is no longer in the JSON — it was `null` in every captured fixture. The `lift-frontend` note disappears from `notes` (the fixtures under `tests/fixtures/captured/mununu-*/` carry it). `diagnostics.frontend` keeps its value. | when the engine pin moves: drop `--frontend slang` and its probe, drop the fallback-reason field and its test (the property it guarded — a fallback is never silent — has no fallback left to guard), re-capture the fixtures. No verdict re-measurement: rosf always forced slang. |
| **monono** | `verify/lint.sh` passes `--frontend slang` (L57); `verify/adequacy.sh` (L33) and `verify/canaries.sh` (L73, the `packidx` canary) pass `--frontend sv2v`: usage errors with the new binary. The `lift-frontend` note that `verify/lib.sh` surfaces inline (L233, L270) is no longer emitted; `verify/selftest-transcript.sh` stubs one (L71–73) and still passes, because it tests the harness, not the engine. `tools/lift-check.py` (L27), `CLAUDE.md` (L763–765) and the skill table's `lift-frontend` row (`.claude/skills/mununu/SKILL.md` L223) describe the `auto` fallback, which no longer exists. | drop `--frontend …` from the three scripts; **re-run the `packidx` canary and the adequacy sweep under slang and record the result** — their sv2v posture is gone and a verdict there is a measurement, not an assumption; retire the `lift-frontend` row. |
| **mununu-ui** | the front-end selector (`SvFrontendSelect`), `lift.frontend` in the `sv/cegar` and `sv/verify-auto` requests, the import tab's `use_sv2v` flag and the diagnostics' fallback line | companion [mununu-ui #85](https://github.com/Mumunu-team/mununu-ui/pull/85) (tsc, eslint and the 285 vitest tests pass); ship it with the engine pin that carries this change. |
| **`verify.toml` projects** | `options.use_sv2v` under a `[[sources]]` entry is a validation error with the message above | remove the key |
| **a yosys without built-in `read_slang`** (< 0.67, or a distribution build without it) | the lift fails with the hint; the fixed-path plugin search is gone | upgrade yosys (the `mununu-sva` image, or oss-cad-suite ≥ 2026-08), or set `MUNUNU_YOSYS_SLANG_PLUGIN=<path to slang.so>` |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes | the binary carries the CLI and API surface above |
| `mununu/docker/Dockerfile.dev` | yes, when the dev workflow runs the new binary | the binary |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | no | tools unchanged: yosys 0.68 with the built-in `read_slang`, slang, Verilator, btormc, pono, cvc5, and sv2v (kept for the `test_support` oracle tests); the binary lives in the cargo-target volume |
| `mununu/docker/Dockerfile.profile` | no | it builds the profiling binary from the mounted tree into the volume; no image content changes |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not lift SystemVerilog |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | yes, together with the adapter change | an image with the new engine and the old adapter fails every verify-auto call |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | no | the engine comes from the cargo-target volume; the lane scripts change, not the images |

## Measured

| run | result |
|---|---|
| `cargo clippy --workspace --all-features --all-targets -- -D warnings`, `cargo fmt --check` | clean |
| `adapter::yosys` driver tests (manifest, script, `read_slang_cmd`, chparam, plugin argument) and `lift_cache` tests | pass on the host (no tool needed) |
| `verify::config` — `options.use_sv2v` refused with the message above | pass |
| `api::schema` drift tests, schemas regenerated | `SvLiftRequest.frontend`, `ContextImportRequest.use_sv2v`, `ModelDiagnosticsView.frontend_fallback_reason` removed; no other shape change |
| mununu-ui companion: `tsc --noEmit`, `eslint --max-warnings 0`, `vitest run` | 0 errors, 0 warnings, 285 tests pass |
| `make e2e-parallel` in `mununu-sva` on this branch | pending — the PR stays draft until this row and the two below are filled |
| verdict-identity gate: `e2e_opentitan_sva_census` on this branch vs `main`, and `diff_corpus_monotone_verdict_ledger` | pending |
| lift-failure count, AssertLLM2 83 designs (private harness) | pending |

## Provenance

- Branch `feat/slang-only-lift`, stacked on #846 (N0); PR pending. Removal R1 of the commercial-gap plan (2026-11-03), Track N step N1.
- Docs: [`docs/verifying-rtl.md`](../verifying-rtl.md) §"The front end is yosys-slang's `read_slang`", [`docs/external-tools.md`](../external-tools.md), [`docs/env-vars.md`](../env-vars.md), [`docs/cli-cookbook.md`](../cli-cookbook.md), [`docs/api-schemas/verify-verbs.md`](../api-schemas/verify-verbs.md), the `sv` wiki pages; the 0.5.1 lift briefing is re-anchored and marked superseded on the fallback line.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The later N1 items — `-DYOSYS` for OpenTitan's own `prim_assert` branch, `opt_clean` before every `write_btor`, the internal-net name map from `write_json` — each lands behind the verdict-ledger gate in its own commit with its own briefing line. The `$check` keep (`setattr -set keep 1 t:$check` before `chformal -lower`) is not coming until yosys-slang can ignore only the SVA it cannot lower: see the "Verdicts" bullet.
- N2 (SPACER removal) and N3 (cvc5 / MathSAT / interpolation / Craig / the `boolector` feature) follow in the same series with their own briefings.
- `scripts/cdc_formal_sby.sh --slang` drives SymbiYosys, not the lift; unchanged.
- The `sv verify` family decides the `bad` nodes of the lifted BTOR2; which assertions become `bad` nodes is unchanged (see "Verdicts").
