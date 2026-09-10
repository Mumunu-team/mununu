---
name: parity-check
description: >
  Verifies a feature is wired consistently across mununu's user-facing
  surfaces — CLI and HTTP API always, plus the UI for CTXDSL-related
  capabilities. Reports drift (a flag with no API peer, a CTXDSL feature
  absent from the UI client) and the inverse case (code added to one surface
  but not exercised by another).
  Use when reviewing changes to user-exposed features, or as a sub-step of
  domain-adequacy and review-orchestrator.
---

Run a CLI ↔ API (↔ UI, for CTXDSL-related capabilities) parity check on $ARGUMENTS (a feature name, file list, or "changed files" if no args).

## Scope determination

If $ARGUMENTS is empty, derive scope from `git diff --name-only HEAD~1 HEAD`. Otherwise, treat $ARGUMENTS as either:
- A feature/flag name (e.g., `--extract-strategy`, `synthesize`, `eval`) — search all three surfaces for it.
- A file path or list — check whether any of the three surfaces touched by those files have peer changes on the other two.

## The surfaces

> **Revised 2026-09-10 (mununu#537).** CLI and API parity is required for **every** capability. UI
> parity is required **only for CTXDSL-related** capabilities — those whose *subject* is a CTXDSL
> model (authoring, evaluating, composing, visualizing, synthesizing from, or importing into one).
> The UI is a CTXDSL workbench.
>
> For anything else — RTL / SVA verification verbs (`sv verify-auto`, `sv lint`, `sv mutate`, the
> `btor2` verbs), contract / black-box tooling, CI-gate ergonomics (exit codes, report formats,
> verdict expectations) — **a missing UI affordance is NOT drift and must not be reported as one.**
> Mark the UI column `n/a`. These features are consumed by CI lanes and orchestrators, not by
> someone at a canvas.

| Surface | Files |
|---|---|
| **CLI** | `crates/mununu-cli/src/main.rs` and `src/main.rs` (clap `Args` structs and command handlers — note that mununu ships *two* binaries named `mununu`; a feature that exists on one but not the other is itself a parity drift) |
| **HTTP API** | `crates/mununu-core/src/api/handlers.rs` (request/response signatures), `crates/mununu-core/src/api/models.rs` (types), `crates/mununu-core/src/api/server.rs` (routes) |
| **UI** *(CTXDSL-related only)* | `mununu-ui/src/api/endpoints.ts` (typed clients), `mununu-ui/src/hooks/useCtxdslEditor.ts`, `mununu-ui/src/hooks/useSummary.ts`, and any other hook/component that consumes the endpoint |

## Known feature groups and where they live

When scope determination identifies a change in one of these groups, search every group member across all three surfaces. Drift inside a group is the most common kind of parity failure.

| Group | CLI subcommand(s) | HTTP route(s) | UI component(s) |
|---|---|---|---|
| **Context core** | `mununu context summarize\|eval\|synthesize\|graph\|merge\|predicates` | `/api/v1/context/{summarize,eval,synthesize,graphs,verify,import,predicates}` | `UnifiedEditor` (Summary / Graphs / Verification tabs) |
| **Extraction** | `mununu extraction validate\|check` | (validation is library-only today) | `ExtractionPanel` |
| **Contracts** | `mununu contract validate\|gaps\|discover\|sidecars\|query\|review` | `/api/v1/contract/{validate,discover,query,review}` | `ContractPanel` (Validate / Discover / Query / Review sub-tabs) |
| **Codesign (HW/SW)** | `mununu codesign couple\|verify` | `/api/v1/codesign/verify` | `CodesignPanel` |
| **SV / RTL** | `mununu sv init\|discover` | `/api/v1/context/import` (with `format=sv-yosys`/`systemverilog`) | Import workflows in `UnifiedEditor` |
| **Templates** | `mununu templates` | `/api/v1/templates` | `TemplatePicker` |

**UI-required groups** (CTXDSL-related): Context core, Extraction, Templates. A new feature in one of these wires all three columns in the same PR.

**CLI + API groups** (not CTXDSL-related): Contracts, Codesign, SV / RTL. A new feature in one of these wires CLI + API; the UI column is `n/a` and its absence is not a finding. Existing UI panels for these groups stay — the obligation to *grow* them is what was removed, not the panels themselves.

A feature added *outside* these groups must land on CLI **and** API, or carry an explicit "CLI-only / API-only" justification under Procedure step 3.

## Procedure

1. Identify which surface(s) the change or feature touches.
2. For each touched surface, search the other two for the corresponding flag/field/route. Use `grep` on:
   - the flag name (`--foo` → `foo` in clap derive)
   - the API field name (snake_case Rust ↔ camelCase TS)
   - the route path (`/context/synth` etc.)
3. For every gap, classify it:
   - **CLI-only**: legitimate if the flag is a developer convenience (e.g., one-shot `--adapter` that the API decomposes into `/import` + `/eval`). Justify in writing.
   - **API-only**: legitimate if the surface is intentionally programmatic (e.g., raw graph dumps consumed only by tooling).
   - **UI-only**: almost always a bug — UI affordances should map to a documented API + CLI verb.
   - **CLI + API but no UI**: **for a CTXDSL-related capability**, drift — report it. For anything else (RTL/SVA, contracts, CI-gate ergonomics) this is the EXPECTED shape: mark the UI column `n/a` and do not report it.
   - **Inverse drift**: code added to one surface but not exercised by either of the others — call out.

## Report format

Emit a Markdown table. The caller (an agent or a review report) can quote it verbatim.

```markdown
### CLI / API / UI Parity

| Change / feature | CLI | API | UI | Drift? | Notes |
|---|---|---|---|---|---|
| `--extract-strategy` | ✓ main.rs:L | ✓ handlers.rs:L | ✗ | YES | CTXDSL-related: API exposes `extract_strategy`; UI client missing |
| `--expect-all-hold` | ✓ main.rs:L | ✓ models.rs:L | n/a | none | CI-gate ergonomics — not CTXDSL-related, so no UI is expected |
| `/context/synthesize` returns `counterstrategy` | ✓ | ✓ models.rs:L | ✓ endpoints.ts:L | none | aligned |
```

Each row must cite a file:line reference for any "✓" entry. "✗" entries should explain whether the gap is intentional (with one-line justification) or unintentional (with a recommended fix).

## When invoked as a sub-step

If the caller is `review-orchestrator` or `domain-adequacy`, return only the Markdown table block — no preamble, no closing summary. The caller embeds it directly under a `### CLI / API / UI Parity` section in its larger report.

When invoked standalone, prepend a one-paragraph executive summary: number of changes in scope, number with drift, number aligned.

## Important

- A pre-existing parity gap discovered during the audit is itself a finding. Report it under the same table even if it predates the current change.
- New examples or workflows added under `examples/` should be reachable from CLI and API (summarizable via CLI, verifiable via API), and from the UI editor as well when the example is a CTXDSL model. If only one surface works, treat it as an incomplete deliverable.
- Do not modify any source file. This skill is read-only.
