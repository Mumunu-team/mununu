# Consumer briefing — the licence file and its status surfaces (Track H1, 2026-11)

> Source of truth: [`licence::status`](../../crates/mununu-core/src/licence.rs#L394) (CLI `mununu licence status`, API `GET /api/v1/licence`, the `licence` field of `/api/v1/health` and of `sv-verify-auto-response`) — surface: CLI+API.

> **Audience:** rosf (runs `sv verify-auto --json` and parses the report), monono (runs the gate through rosf and reads the same report), mununu-ui (reads `/api/v1/health`), and anyone deploying `mununu server` on their own machines.
>
> **Related:** [`docs/licensing.md`](../licensing.md); [`docs/api-schemas/licence-status.schema.json`](../api-schemas/licence-status.schema.json); the build-identity briefing (mununu#715), whose channel the licence state rides.
>
> **TL;DR — a new route, a new field on two existing documents, no verdict change, no behaviour change.** mununu now reads a signed offline licence file (`mununu.lic`) and reports its state. Every report gains `licence: {state, tier?, expires?}` next to `build`; `/api/v1/health` gains the same object; `GET /api/v1/licence` and `mununu licence status` give the full status. Without a file the state is `missing` and **everything runs exactly as before**: no engine reads the licence, and the verdicts of a design are byte-identical in every licence state. What the tool does in each state beyond reporting it is a separate decision; it will ship with its own briefing.

## What changed

| surface | before | after |
|---|---|---|
| `POST /api/v1/sv/verify-auto` response and `sv verify-auto --json` | `build`, `build_clean` | plus `licence: {"state": "valid" \| "grace" \| "expired" \| "invalid" \| "missing", "tier"?: "evaluation" \| "team", "expires"?: "YYYY-MM-DD"}` — always present |
| `GET /api/v1/health` | `{status, service}` | plus `licence` (the same three-field object) |
| `GET /api/v1/licence` | — | new: the full status (`state`, `source`, `claims`, `days_left`, `reason`, `note`), schema `licence-status.schema.json` |
| `mununu licence status [--json]` (alias `license`) | — | new: the same document, or a human-readable block; exits 0 in every state |
| `mununu.toml` | `[licensing]` parsed and ignored | `[licensing] file = "mununu.lic"` names the file; any other key under `[licensing]` is refused (it was never read) |
| environment | — | `MUNUNU_LICENSE_FILE` names the file when `mununu.toml` does not; then `/etc/mununu/mununu.lic` |

The schema `sv-verify-auto-response.schema.json` is regenerated for the new required field.

## Per consumer

| consumer | what this means for you | action |
|---|---|---|
| **rosf** | Every parsed report carries `licence.state`; your gate runs without a file and sees `missing`. Nothing else in the document moved. | None required. If the report model is strict (unknown fields refused), add the `licence` object; if it records `build`, record `licence.state` beside it — a verdict's provenance now includes the licence state it was produced under. |
| **monono** | Same report, through rosf. | None. |
| **mununu-ui** | `/api/v1/health` has a `licence` object; the Settings surface planned for Track P reads it. | None now; the field is additive. |
| **self-hosted deployments** | `mununu licence status` says whether the file you installed is read, by which key, and until when. Put the file at `/etc/mununu/mununu.lic`, or name it in `mununu.toml` or `MUNUNU_LICENSE_FILE`. | Install the file when you have one; nothing changes until then. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` / the release images | Yes, to get the route and the field; nothing breaks without it. | A binary change (new route, new report field). An older image reports no `licence` field and has no `/api/v1/licence`. |
| rosf `docker/Dockerfile*` | No. | rosf consumes the report; the field is additive. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | No. | Same. |

## Provenance

- Roadmap Track H, item H1 (the delivery lane's L1); the file format, locations and surfaces are as the roadmap specified. The falsifier the roadmap set — a fixed design's verdicts byte-identical across every licence state — is held structurally by `crates/mununu-core/tests/licence_never_reaches_a_verdict_h1.rs` (no verdict module references the licence) and behaviourally by the unit tests in `licence.rs` and the report-model test in `api/models.rs`.
- The verifier accepts RFC 8032 signatures (test vector in `licence.rs`), so the private issuer (H2) and the binary agree by construction.

## Not covered here

- Issuance (Track H2, private): how a licence file is produced and delivered.
- The behaviour per state beyond reporting it (the roadmap's recommendation table — run, warn, refuse to start past grace): a separate decision and PR, with its own briefing.
- Pricing, evaluation length and the public-licence reconciliation (H4).
