# mununu API — machine-readable JSON Schemas

> **mununu#536 — one shape, two surfaces.** `sv-verify-auto-response.schema.json`
> describes the HTTP response **and** the CLI's `mununu sv verify-auto --json`
> output: both go through the single `impl From<&AutoVerifyReport> for
> SvVerifyAutoResponse` in `crates/mununu-core/src/api/models.rs`, so the drift test
> below covers both. There used to be two hand-written serializers for this shape and
> they had diverged; a consumer reading this schema and then running the CLI got a
> different document.

This directory holds the **machine-readable** contract for mununu's HTTP API wire types, alongside the human-facing prose in [`verdict.md`](verdict.md).

Each `*.schema.json` file is a JSON Schema (Draft-07) derived directly from the Rust source via `#[derive(schemars::JsonSchema)]` on the API types in [`crates/mununu-core/src/api/models.rs`](../../crates/mununu-core/src/api/models.rs). The generator lives at [`crates/mununu-core/src/api/schema.rs`](../../crates/mununu-core/src/api/schema.rs); the drift-detector at the bottom of that file's `#[cfg(test)]` block asserts every commit that the shipped file matches the current Rust types — a wire-format change without a schema update fails CI.

## Files

### The flagship (BTOR2-free SV entry)

| File | Endpoint | Direction |
|------|----------|-----------|
| [`sv-verify-auto-request.schema.json`](sv-verify-auto-request.schema.json) | `POST /api/v1/sv/verify-auto` | Request body |
| [`sv-verify-auto-response.schema.json`](sv-verify-auto-response.schema.json) | `POST /api/v1/sv/verify-auto` | Response body |

### BTOR2-direct property verbs

| File | Endpoint | Direction |
|------|----------|-----------|
| [`btor2-verify-request.schema.json`](btor2-verify-request.schema.json) | `POST /api/v1/btor2/verify` | Request body |
| [`btor2-verify-response.schema.json`](btor2-verify-response.schema.json) | `POST /api/v1/btor2/verify` | Response body |
| [`btor2-verify-liveness-request.schema.json`](btor2-verify-liveness-request.schema.json) | `POST /api/v1/btor2/verify-liveness` | Request body |
| [`btor2-verify-liveness-response.schema.json`](btor2-verify-liveness-response.schema.json) | `POST /api/v1/btor2/verify-liveness` (also `-all`) | Response body |
| [`btor2-verify-liveness-all-request.schema.json`](btor2-verify-liveness-all-request.schema.json) | `POST /api/v1/btor2/verify-liveness-all` | Request body |
| [`btor2-verify-liveness-under-fairness-request.schema.json`](btor2-verify-liveness-under-fairness-request.schema.json) | `POST /api/v1/btor2/verify-liveness-under-fairness` | Request body (response = `btor2-verify-liveness-response.schema.json`) |
| [`btor2-verify-recoverability-request.schema.json`](btor2-verify-recoverability-request.schema.json) | `POST /api/v1/btor2/verify-recoverability` | Request body |
| [`btor2-verify-recoverability-response.schema.json`](btor2-verify-recoverability-response.schema.json) | `POST /api/v1/btor2/verify-recoverability` | Response body |
| [`btor2-check-fsm-request.schema.json`](btor2-check-fsm-request.schema.json) | `POST /api/v1/btor2/check-fsm` | Request body |
| [`btor2-check-fsm-response.schema.json`](btor2-check-fsm-response.schema.json) | `POST /api/v1/btor2/check-fsm` | Response body |

### SV-wrapped verbs (lift SV → BTOR2, then decide — response shape is the BTOR2 sibling)

| File | Endpoint | Direction | Response schema |
|------|----------|-----------|-----------------|
| [`sv-verify-request.schema.json`](sv-verify-request.schema.json) | `POST /api/v1/sv/verify` | Request body | `btor2-verify-response.schema.json` |
| [`sv-verify-liveness-request.schema.json`](sv-verify-liveness-request.schema.json) | `POST /api/v1/sv/verify-liveness` | Request body | `btor2-verify-liveness-response.schema.json` |
| [`sv-verify-liveness-all-request.schema.json`](sv-verify-liveness-all-request.schema.json) | `POST /api/v1/sv/verify-liveness-all` | Request body | `btor2-verify-liveness-response.schema.json` |
| [`sv-verify-recoverability-request.schema.json`](sv-verify-recoverability-request.schema.json) | `POST /api/v1/sv/verify-recoverability` | Request body | `btor2-verify-recoverability-response.schema.json` |
| [`sv-check-fsm-request.schema.json`](sv-check-fsm-request.schema.json) | `POST /api/v1/sv/check-fsm` | Request body | `btor2-check-fsm-response.schema.json` |

More endpoints will land in additive PRs (CEGAR + synth) — see the roadmap in `docs/consumer-briefings/`.

Narrative companion: [`verify-verbs.md`](verify-verbs.md) walks the BTOR2 + SV verb responses field-by-field and explains where each verb sits on the safety / liveness / recoverability / FSM axes.

## Consumer flow

1. Pin a specific mununu release + fetch the corresponding schema files.
2. Point your codegen at them:
   - **TypeScript**: `openapi-typescript` or `quicktype --lang typescript --src-lang schema`
   - **Python**: `datamodel-code-generator --input schema.json --input-file-type jsonschema --output types.py`
   - **Rust**: `typify` or `schemars` on the consumer side re-parsing the schema
   - **Java / Go / Kotlin / …**: `quicktype` supports all of them
3. On a mununu binary bump, either accept the drift (the schema in the new release is the new contract) or pin the previous release while you migrate.

## What is guaranteed

- Every type documented in [`verdict.md`](verdict.md) appears in the response schema (the `response_schema_references_documented_types` test guards against silent removal).
- Field names, types, required-vs-optional markers, and nested-type references all match the Rust source. If the Rust changes, CI fails until the schema file is regenerated.
- The schema is Draft-07 (`$schema: "http://json-schema.org/draft-07/schema#"`), which every mainstream codegen accepts.

## What is NOT guaranteed

- **Enum-value constraints on `outcome` / `kind` / `level`.** These fields are typed as `String` in the Rust source (for backward-compat with existing consumers) rather than as enums, so the schema only declares `type: "string"` without an `enum` clause. The allowed values live in [`verdict.md`](verdict.md) — treat them as informational, tolerate unknown values (mununu's stability contract permits additive expansion).
- **Cross-endpoint semantic invariants** (e.g. "when `outcome == violated`, `counterexample` may be present"). The schema documents the shape; the meaning lives in `verdict.md`.

## Updating the schemas

When a wire-format change lands in the Rust source, CI fails on the drift-detector test. Fix:

```sh
MUNUNU_UPDATE_API_SCHEMAS=1 \
  cargo test -p mununu-core --lib --features api api_schema_drift_ -- --test-threads=1
git add docs/api-schemas/
```

Then ship a consumer briefing per [`../policies/cross-repo-impact.md`](../policies/cross-repo-impact.md) explaining the transition. The briefing is a required companion for any wire-format shape change (additive is safe to ignore for consumers; breaking is not).

## Why not OpenAPI / `utoipa`?

For now, downstream consumers want machine-readable per-response schemas, not a full `/openapi.json` endpoint with axum handler integration. `schemars` is the lighter dependency and its output composes cleanly with every JSON-Schema-native codegen. A future `utoipa` layer can co-exist — it just consumes the same derived `JsonSchema` impls.

## See also

- [`verdict.md`](verdict.md) — the human-facing narrative on `PropertyVerdict`, response fields, counterexample shapes, and consumer guidance.
- [`../design/antecedent-shadow-synthesis.md`](../design/antecedent-shadow-synthesis.md) — how a `skipped` verdict can flip to a definite one after enabling shadow-synth.
- [`../policies/cross-repo-impact.md`](../policies/cross-repo-impact.md) — the policy that requires a consumer briefing on any wire-format change.
