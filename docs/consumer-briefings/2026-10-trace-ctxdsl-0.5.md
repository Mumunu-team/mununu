# Consumer briefing — 0.5 `Trace`, part 2: the CTXDSL paths (2026-10)

> Source of truth: [`Trace::from_states`](../../crates/mununu-core/src/trace.rs) and the serializers in [`crate::trace::ser`](../../crates/mununu-core/src/trace.rs) — surface: (CLI+API+UI)

**Audience:** mununu-ui above all (its lasso, deadlock and sanity renderers), plus monono and every client that reads CTXDSL synthesis diagnostics or the `context eval --sanity` report. Second part of the 0.5 `Trace` cluster ([`2026-10-trace-type-0.5.md`](2026-10-trace-type-0.5.md)).

## TL;DR

- **Every CTXDSL path is a `Trace`.** Steps carry `state_name` and `labels` (the labels of the transition out of that step); a lasso sets `loop_start`. The RTL verbs use the same type with register cells, so one renderer serves both.

  | field | before | after |
  |---|---|---|
  | synthesis `diagnostics.counterexample_trace` (API `/context/synthesize`, `context synth --dump-json`) | `["idle", "req1", …]` | **renamed** `counterexample`: a `finite` trace |
  | `diagnostics.counterstrategy_traces`, `diagnostics.deadlock_traces` | `[["s0", "s1"], …]` | `[Trace, …]` (`finite`) |
  | `diagnostics.lasso_traces` | `[{prefix, cycle, prefix_labels, cycle_labels}]` | `[Trace, …]` (`lasso`, `loop_start` = the old `prefix.length`, labels per step) |
  | sanity report `deadlocks[]` (`context eval --sanity --json`, `/context/verify` with `sanity`) | `{states, labels}` | a `finite` trace |

- **The CLI and the API emit the same form.** The core diagnostics serialize through `crate::trace::ser` and `LassoTrace` / `SanityPath` serialize as traces, so `--dump-json` and the API agree. The text output (`context synth`, `context eval --sanity`) is unchanged.
- **Unchanged:** the `.ctxdsl` diagnostics sidecar (`--dump-diagnostics`), verdicts, and the counterstrategy graph (`counterstrategy`, a view).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **mununu-ui** | `LassoTraceViewer` (`prefix` / `cycle` / `prefix_labels`), `DeadlockTraceViewer` (string lists), `useVerification`'s `counterexample_trace` / `deadlock_traces` / `lasso_traces`, and the sanity panel's `deadlocks[].states` / `.labels` | read `steps[].state_name` / `steps[].labels` / `loop_start`; one trace renderer can replace the lasso and deadlock viewers |
| **monono** | lanes that read synthesis diagnostics JSON | read `counterexample.steps[].state_name` |
| **rosf** | none | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the shape lives in the binary |
| `mununu/docker/Dockerfile.dev` | yes, for the UI's dev backend | mununu-ui reads these fields |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | monono lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | no | profiling scripts read verdicts, not traces |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not emit traces |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | no | not consumed by rosf |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | diagnostics reads |

## Verification steps for every consumer

```bash
mununu context synth examples/wiki/unrealizable_valve.ctxdsl --automaton Valve --formula … --dump-json out.json \
  && jq '.diagnostics.counterexample.kind, (.diagnostics.lasso_traces[0].loop_start)' out.json
mununu context eval examples/wiki/door_lock.ctxdsl --automaton DoorLock --sanity --json | jq '.sanity[0].deadlocks'
```

## Provenance

- Branch `feat/trace-ctxdsl-0.5`. The `Trace` types moved to the ungated core module `crate::trace` (re-exported by `api::models`) so the core diagnostics can serialize them. Tests: `controller_diagnostics_exports_reports` (the dump now has `counterexample.kind == "finite"`), the context suite (42), the handler suites under all features, the legacy `cli_session` suite, and the `sterile_batch_release` example build.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The mununu-ui migration itself, which lands with the 0.5 consumer PRs.
