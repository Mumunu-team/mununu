# Consumer briefing — template packs and the deployment configuration (2026-10)

> Source of truth: [`TemplateRegistry::load`](../../crates/mununu-core/src/adapter/templates/mod.rs) + [`DeploymentConfig`](../../crates/mununu-core/src/deployment.rs) — surface: (CLI+API) — `mununu templates`, `context eval|synth --template`, `verify`, `GET /api/v1/templates`, every template reference

**Audience:** mununu-ui, monono, rosf, and whoever operates a mununu server.

## TL;DR

- **New, opt-in:** template packs layered over the built-in catalog — system (`/etc/mununu/templates.d/*.json`), deployment (`mununu.toml` `[templates] files`), project (`verify.toml` `[templates] files`, `--templates-file`). A later layer overrides an earlier one by template `id`.
- **New:** `mununu.toml` (global `--deployment-config FILE`, `mununu server --config FILE`, or `MUNUNU_DEPLOYMENT_CONFIG`): `enabled_domains` restricts the templates a deployment exposes; `[licensing]` is reserved. A pack's `entitlement` is reported, not enforced.
- **Response changes on `GET /api/v1/templates`:** each template carries `origin` (`layer`, `pack`, `file`, `entitlement`); results are sorted by `id`; an unknown `?domain=` is a **400** (it used to return the full catalog silently).
- **CLI changes on `mununu templates`:** with no `--domain` it lists every template (it listed only the universal-tagged ones although the code said "all"); `--json` follows the `--domain` filter (it ignored it); each row shows its origin.
- **Instance names are deterministic:** a template with two or more parameters produced its property name from a `HashMap` walk, so the name could differ between runs; it now follows the template's parameter order (`response_TRIGGER_x_RESPONSE_y`).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | none (no template use) | none |
| **monono** | `verify.toml` template properties keep working; property names of multi-parameter templates are now stable (they may differ from a name pinned by an earlier run) | re-pin any `--expect` on a multi-parameter template name |
| **mununu-ui** | `GET /api/v1/templates`: new `origin` field, sorted output, unknown domain → 400 | show `origin`; handle the 400 |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes, to use packs or `server --config` | new flags and loader |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | no | templates are not on the RTL path yet |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | no | no template use |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | stable template names |

## Measured

| run | result |
|---|---|
| `layer_tests` (4) + `deployment::tests` (2) | layers override by id and report origin; `enabled_domains` keeps one domain plus universal; a broken pack names its file; instance names are stable over 8 runs; unknown domains and unknown `mununu.toml` keys are refused |
| CLI end to end (`--deployment-config`, `MUNUNU_DEPLOYMENT_CONFIG`, `--templates-file`, a missing config file) | the pack's template is listed as `deployment (acme-rtl)`, its agentic sibling filtered out by `enabled_domains = ["rtl"]`; the project layer shows `project (acme-rtl)`; a missing config file is an error |
| `cargo test -p mununu-core --features api --no-fail-fast`, `cargo test -p mununu-cli` | pass |

## Provenance

- Commit: pending merge — branch `feat/template-catalog-layers`.
- Docs: [`wiki/Property-Templates.md`](../../wiki/Property-Templates.md) §"Template packs and the deployment configuration", [`docs/env-vars.md`](../env-vars.md).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- RTL templates on `sv verify-auto` (`--template`, `RtlAtom` parameters) — the next PR.
- Signed packs, license keys and the usage ledger (reserved `entitlement` / `[licensing]`).
