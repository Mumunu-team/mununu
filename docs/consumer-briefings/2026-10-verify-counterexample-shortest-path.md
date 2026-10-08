# Consumer briefing — 2026-10 `verify` counterexamples: a shortest path to the violating state, a configurable cap, the target named even when truncated

> **Audience:** anyone running `mununu verify --print-counterexample` or reading `VerifyReport.property_verdicts[].counterexample` from the CLI JSON or `POST /api/v1/verify` — and anyone parsing the report's shape.
>
> **Provenance:** [mununu#594](https://github.com/Mumunu-team/mununu/issues/594). Code: `verify/orchestrator.rs` (`build_counterexample_witness`, `safety_body_violations`, `shortest_path_witness`), `verify/config.rs`, `verify/report.rs`, the CLI flag, the API request field. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## TL;DR

For the safety shape `nu X. (phi && [] X)` — the `never` template and most hand-written invariants — the counterexample is now the **shortest path** (BFS over the product) from a violating initial state to a `!phi` state, and the witness names that state (`violating_state`) even when the printed steps are cut at the cap. Before, a forward walk preferring violating successors could spend its 20 steps on unrelated components' self-cycles and end in `(length-limit (truncated))` without ever showing the state the property was about. Other formula shapes keep the walk. The cap is configurable.

No verdict changes. The witness changes shape for safety properties (shorter, ends at `!phi`), and the report gains two fields.

## What changed

- **Report:** `VerifyReport.counterexample_max_steps: usize` (the cap this run used; 20 by default) and `TraceWitness.violating_state: Option<String>` (absent on the non-safety shapes; `skip_serializing_if = None`). Both deserialise with defaults for older readers.
- **Config:** `counterexample_max_steps = N` at the top level of `verify.toml`.
- **CLI:** `mununu verify --counterexample-max-steps <N>` overrides the config; the human printer says `length-limit (truncated at N steps; raise with --counterexample-max-steps)` and `violating state: …` / `leads to violating state: …`.
- **API:** `counterexample_max_steps` on the verify request overrides the config.

## What to update, per consumer

- A parser pinned to the exact JSON shape of `VerifyReport` or `TraceWitness` sees two new keys; both are additive.
- A script that grepped the trace for the violating state can read `violating_state` instead; on the safety shape it is always present.
- The default cap is unchanged (20), so transcripts of walks on non-safety shapes are byte-identical; safety-shape traces are now shortest paths and will differ (shorter, and ending at the `!phi` state).

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | report shape, CLI flag, API field, witness content | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu; does not run `verify` on CTXDSL | No |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
cargo test -p mununu-core --lib -- x594_
mununu verify --print-counterexample --counterexample-max-steps 40 verify.toml
```

## Not covered here

- Non-safety shapes (liveness, nested fixpoints) keep the forward walk; a shortest-path witness for `AG EF` needs a lasso, not a path.
