# Consumer briefing — 2026-10 the recoverability verbs now say who decided (`engine`) and why the exact engine stepped aside (`exact_abstention`)

> **Audience:** anyone parsing `btor2 verify-recoverability` / `sv verify-recoverability` output — the CLI JSON documents and the API responses of `POST /api/v1/btor2/verify-recoverability` and `POST /api/v1/sv/verify-recoverability`. ROSF's recoverability lane and monono's formal gate read these.
>
> **Related:** tracking issue [mununu#682](https://github.com/Mumunu-team/mununu/issues/682); the bit-cap frontier measurement (`scripts/bitcap_frontier.py`, EXP-0031); `sv verify-auto` already carried the same attribution as `decided_by` (mununu#656 follow-up). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **JSON-shape addition, no verdict change.** Two optional keys on the recoverability documents, both surfaces: `engine` (`exact` | `exact-reach-restricted` | `cube-ladder`) and, only when the cube ladder decided, `exact_abstention` (`bit-cap`, `arena-overflow`, `iteration-budget`, `latency`, `arena-safety`, `deadline`, `cancelled`, `refused-input-atom`, `reach-declined`, `other`). Verdicts, refinements and exit codes are unchanged. A consumer that ignores unknown keys needs no change; a consumer that validates against the committed schema must take the regenerated schema.

## What changed

The recoverability verbs computed which rung produced the verdict (`RecoverabilityEngine`) and then threw it away (`verify_recoverability_with_predicates` mapped it off), and the exact engine's abstention reason went to a `warn!` that `--quiet` filters. Two `"unknown"` documents were therefore indistinguishable: the exact engine never admitted the cone (bit cap) vs. it ran out of nodes (arena overflow) vs. the cube ladder itself abstained. A run killed from outside could not be attributed at all.

- `adapter/recoverability.rs` — `verify_recoverability_explained` and `verify_recoverability_refined_explained` return a `RecoverabilityAttribution { engine, exact_abstention, exact_abstention_reason }`; the existing functions are thin wrappers, so `sv verify-auto`'s `decided_by` is untouched.
- `adapter/btor2/symbolic_bitblast.rs` — `AbstentionKind::label()`, one kebab-case string per variant, pinned distinct by a unit test.
- `api/models.rs` — `Btor2VerifyRecoverabilityResponse` and `SvVerifyRecoverabilityReport` gain the two optional fields (`skip_serializing_if = Option::is_none`); both committed schemas regenerated.
- CLI `btor2 verify-recoverability` adds the same two keys to its document; `sv verify-recoverability` fills the typed report.

## What to expect

```jsonc
{ "file": "link_ctrl.sv", "property": "AG EF (state_q == 0)", "verdict": "holds",
  "engine": "exact" }                                   // the exact engine decided

{ "file": "mult48.btor2", "property": "AG EF (done == 1)", "verdict": "unknown",
  "engine": "cube-ladder", "exact_abstention": "arena-overflow" }   // exact gave up, the ladder abstained too
```

`exact-reach-restricted` is the normal label whenever the exact run outlived the ladder's first 2-second slice; its verdict is identical to `exact` by construction. `exact_abstention` never appears beside `engine: exact*`.

## What to update, per consumer

- **ROSF** (`rosf verify`, recoverability objective): the report parser may now read `engine` into its `decided_by`-style provenance; nothing breaks if it does not. If ROSF validates responses against `docs/api-schemas/*.schema.json`, vendor the regenerated files.
- **monono** (`make formal`, the recoverability rows): no change to verdicts or exit codes; the `[mununu#553]` peak line is unchanged. Optional: record `engine` per row in `metrics.md` so a row that moves from `exact` to `cube-ladder` is visible.
- Anyone counting abstentions: `exact_abstention == "bit-cap"` is the only kind a cap raise fixes; `arena-overflow` and `iteration-budget` are shape-bound (see the frontier table in `docs/verifying-rtl.md` once published).

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | new JSON keys on the recoverability verbs | **Yes** |
| `mununu-dev` | test image; carries the schema drift tests | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** (to see the keys; not required for correctness) |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
# 1. The labels and the handler, no tools:
cargo test -p mununu-core --lib --features api -- abstention_kind_labels_are_distinct btor2_verify_recoverability_handler_detects_absorbing_trap api_schema_drift_
# 2. Any fixture, both surfaces:
mununu btor2 verify-recoverability examples/btor2/<fixture>.btor2 --target "state == 0" | jq '{verdict, engine, exact_abstention}'
curl -s localhost:8080/api/v1/btor2/verify-recoverability -H 'content-type: application/json' -d @req.json | jq '{verdict, engine, exact_abstention}'
```

## Not covered here

- The exact engine's abstention *message* (`exact_abstention_reason`) is kept in the Rust attribution but not serialised — the typed kind is the stable surface; the message wording is not.
- `sv verify-auto`'s `decided_by` label is unchanged (it carries the long form, e.g. `"exact-symbolic (recoverability rescue)"`).
