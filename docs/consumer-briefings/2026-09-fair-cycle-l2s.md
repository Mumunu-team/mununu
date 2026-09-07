# Consumer briefing — 2026-09 fair-cycle l2s primitive + `verify-liveness-under-fairness` verb (mununu#477 Option B, PR 1)

> **Audience:** ROSF (API consumer via `--profile industrial`), monono (direct CLI + API consumer), any orchestrator that wants to decide `AG(request → AF grant)` under a `GF` environment fairness assumption.
>
> **Related:** [mununu#477](https://github.com/Mumunu-team/mununu/issues/477) — the ticket asking for fairness-constrained MC on the SV path. This is **PR 1 of 2** for Option B: the standalone primitive + a `btor2`-direct verb. **PR 2** wires this into `sv verify-auto` so `// @mununu_assume GF x` is auto-applied on the SV path.
>
> **TL;DR:** new `mununu btor2 verify-liveness-under-fairness` verb + `POST /api/v1/btor2/verify-liveness-under-fairness` endpoint decide `(⋀ⱼ GF fairⱼ) → AG(request → AF grant)` via the Emerson–Lei fair-cycle extension of the plain l2s. **Purely additive** — existing `verify-liveness` behaviour is byte-for-byte unchanged (guarded by a regression test). Consumers who parse the plain `verify-liveness` response can consume the new verb with zero shape changes (same `Btor2VerifyLivenessResponse`).

## What changed

- **New engine primitive** — `emit_response_l2s_monitor_under_fairness` in [`crates/mununu-core/src/adapter/btor2/l2s_monitor.rs`](../../crates/mununu-core/src/adapter/btor2/l2s_monitor.rs). Additive sibling of `emit_response_l2s_monitor`; empty `fairness_atoms` slice recovers the plain emitter byte-for-byte (guarded by `empty_fairness_matches_plain_emitter_byte_for_byte`).
- **New library entry** — `response_liveness_rescue_under_fairness` in [`crates/mununu-core/src/adapter/liveness_rescue.rs`](../../crates/mununu-core/src/adapter/liveness_rescue.rs), plus a `parse_fairness_atoms` helper.
- **New CLI verb** — `mununu btor2 verify-liveness-under-fairness --request R --grant G --fairness F1 --fairness F2 <btor2>`. Repeatable `--fairness`; empty recovers `verify-liveness` semantics.
- **New API endpoint** — `POST /api/v1/btor2/verify-liveness-under-fairness`. Request type `Btor2VerifyLivenessUnderFairnessRequest` (new); response type is the existing shared `Btor2VerifyLivenessResponse` (no new response type — verdict / property / decided_by).
- **New UI client** — `runBtor2VerifyLivenessUnderFairness` in `mununu-ui/src/api/endpoints.ts` (sibling repo). Uses the extended (`aiApiClient`, 120s) client since the primitive is Z3/subprocess-heavy on nontrivial designs.
- **New JSON schema file** — `docs/api-schemas/btor2-verify-liveness-under-fairness-request.schema.json`. Drift-detector row + schema-derivation function added.
- **Documentation** — `docs/verifying-rtl.md` gains the verb section; `docs/api-schemas/verify-verbs.md` gains the endpoint row; `docs/api-schemas/README.md` lists the new schema file.
- **Soundness tests** (7 pass in [`crates/mununu-core/tests/fair_cycle_l2s.rs`](../../crates/mununu-core/tests/fair_cycle_l2s.rs) + 4 unit tests in the l2s_monitor module):
  1. Positive rescue — `GF fair` rescues a starving design.
  2. Regression — an already-holding design stays holding under any fairness.
  3. **Useless-fairness soundness control** — `GF(dead_input == 0)` does NOT rescue a genuinely starving design (guards against the Emerson–Lei bug where a bad guard makes every `fair_seen` trivially true).
  4. Zero-fairness equivalence — the fair-cycle entry with `&[]` matches the plain entry.
  5-7. Multi-conjunction TWOGATE analog — neither `GF fair_1` nor `GF fair_2` alone rescues, both together do.

## What did NOT change

- **Wire format** — every existing endpoint and response shape unchanged.
- **`verify-liveness` behaviour** — byte-for-byte identical on the emitted l2s monitor (guarded by a byte-equivalence test).
- **`PropertyVerdict` values** — same canonical vocabulary.
- **Existing schema files** — no diff on any existing `docs/api-schemas/*.schema.json`.

## For ROSF and monono

**What to update:** nothing forced. To adopt the new verb:

```sh
# CLI:
mununu btor2 verify-liveness-under-fairness design.btor2 \
    --request "req == 1" --grant "ack == 1" \
    --fairness "grant_cpu == 1"

# HTTP API:
curl -X POST http://localhost:3000/api/v1/btor2/verify-liveness-under-fairness \
    -H "Content-Type: application/json" \
    -d '{
      "content": "<btor2 source>",
      "request": "req == 1",
      "grant": "ack == 1",
      "fairness": ["grant_cpu == 1"]
    }'
```

The response reuses `Btor2VerifyLivenessResponse` — same `verdict` / `property` / `decided_by` shape. Empty `fairness` reduces to `verify-liveness` exactly, so a consumer can safely default to the fair-cycle verb everywhere and pass `fairness: []` on the non-fairness path.

**Soundness contract:**

- A `holds` verdict under `⋀ⱼ GF fairⱼ` means the response property holds against every environment schedule that satisfies each fairness constraint infinitely often.
- A `violated` verdict means there exists a reachable lasso satisfying every fairness constraint AND leaving a request forever ungranted.
- A useless fairness atom (one env satisfies trivially — e.g., a dead input the design never reads set to 0) does NOT change the verdict. The soundness control test validates this.
- The atoms follow the same shape as `verify-liveness`: `REG op VALUE` single-register comparisons, resolved via the same `state_nid_and_sort` / `input_nid_and_sort` / `output_nid_and_sort` lookup as the ante / cons atoms.

**Docker rebuild:** binary bump only; no subprocess tool changes.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod) | binary picks up new verb + endpoint + engine entry | Yes if consumers pin a version tag |
| mununu `Dockerfile.dev` | binary bump + 7 new soundness tests + 4 new unit tests | Only if the dev workflow requires the new binary |
| mununu `Dockerfile.sva` | binary bump; no e2e slang-gated behaviour change | No — the ignored SVA e2e set is unaffected |
| mununu `Dockerfile.extract`, `.extract-*` | no impact | No |
| rosf `Dockerfile` / `Dockerfile.dev` / `.hw` | consumes mununu CLI/API; opt-in feature only | No — behaviour updates on next binary pull; adoption is optional |
| monono Docker (if any) | primary beneficiary — `wb_mem_client`-shaped cases become decidable | No — pull the new binary and start passing `--fairness` |
| mununu-ui deployment | new client function available; existing UI unaffected | No — deploy the new bundle if a UI component consumes it |

## Verification steps

- `cargo test -p mununu-core --test fair_cycle_l2s` — 7 soundness tests pass.
- `cargo test -p mununu-core --lib l2s_monitor` — 7 unit tests pass (byte-equivalence + structural).
- `MUNUNU_UPDATE_API_SCHEMAS=1 cargo test -p mununu-core --lib --features api api_schema_drift_ -- --test-threads=1` — regenerates all schemas idempotently; the new drift-detector row (`api_schema_drift_btor2_verify_liveness_under_fairness_request`) passes.
- `mununu btor2 verify-liveness-under-fairness --help` prints the new verb; `--fairness` accepts repeatable atoms.
- End-to-end on a hand-authored BTOR2 with a rescue-shape design: expect `holds` under a matching `GF fair`; expect `violated` with an empty or useless fairness list.

## Provenance

- Fix commit: (pending merge — branch `feat/477-b-fair-cycle-l2s-primitive`).
- Ticket: [mununu#477](https://github.com/Mumunu-team/mununu/issues/477).
- Prior briefing on this track: [`2026-09-fairness-note-honesty.md`](2026-09-fairness-note-honesty.md) — Option A note honesty fix.
- Follow-up: PR 2 will wire `// @mununu_assume GF x` on `sv verify-auto` to auto-dispatch to this primitive.
- Design record: agent-side plan doc `.claude/plans/477-b-fair-cycle-l2s.md`.
- Policy: [`../policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here (follow-ups)

- **PR 2 — the SV verify-auto bridge.** Detect `GF <atom>` in `@mununu_assume` annotations; for guarantees with response shape, auto-dispatch to `response_liveness_rescue_under_fairness`; fold the verdict into the property row via `holds_under` with `AssumptionKind::InputFairness`. Closes #477.
- **Full Streett shape `⋀_i AG(a_i → AF b_i)` under coupled fairness.** The current primitive handles a single response under conjunctive justice; the multi-response coupled shape (where fairness constraints couple the guarantees) is a further follow-up.
- **General LTL assumptions.** The ticket explicitly narrows to `GF <signal>` conjunctions; broader LTL is out of scope.
- **`sv verify-liveness-under-fairness`** SV-wrapped verb. Not shipped in this PR since the SV auto-routing bridge in PR 2 supersedes it for the main use case; can be added additively if a consumer needs it directly.
