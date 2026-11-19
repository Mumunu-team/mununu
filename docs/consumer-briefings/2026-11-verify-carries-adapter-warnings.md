# Consumer briefing: `mununu verify` carries each adapter's warnings in the report (2026-11)

> Source of truth: [`SourceSummary::warnings`](../../crates/mununu-core/src/verify/report.rs) and [`dispatch_adapter`](../../crates/mununu-core/src/verify/orchestrator.rs) — surface: CLI+API — `mununu verify` / `mununu verify --inspect` (text: `warning:` lines under each source; `--json`: `sources[].warnings`), and the API routes that run the N-source orchestrator and return its report

**Audience:** anyone who reads a `mununu verify` or inspection report, by eye or as JSON — in particular a gate that reads `property_verdicts` and never looked at the log.

## TL;DR

- **New field, additive (mununu#1106 item 3).** Each entry of `sources[]` may now carry `warnings: string[]`: what the adapter said it did less than asked while translating that source — a construct it dropped, a variable it left out, an approximation it took — in the adapter's own words. Until now `dispatch_adapter` kept the CTXDSL and the partition summary and **dropped** the `AdapterWarning`s for the crewai, langgraph, microcode, extraction and xstate routes; the sv-yosys route and the predicate-cube lift logged theirs at `tracing::warn!` only. A verdict over a model missing a piece read as a verdict over the whole.
- **What travels:** the adapter's `warnings` (every route through `FormatAdapter::translate`), the sv-yosys sidecar lint's findings, the bit-blaster's warnings on the sv-yosys route, and the predicate-cube lift's warnings (a predicate it could not bind, an unestablished initial cube, …). Order: lint first, then the lift / bit-blaster.
- **No verdict changes.** The models and verdicts are the same; what changes is that the reading context is on the report. `warnings` is absent when the translation was complete (`skip_serializing_if = Vec::is_empty`), so a client that does not look for it sees the old shape.
- **CLI text:** under each `- <id> (adapter = …)` line, one `      warning: …` line per entry, on both `verify` and `--inspect`.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A `verify` gate over an xstate / extraction / agentic source gets `sources[].warnings` when the adapter left something out. Treat a non-empty list as "the verdict is over a model missing what these lines name". | Surface the list beside the verdicts; stable per-adapter prefixes, free text otherwise. |
| **monono** | Same, on the sv-yosys / predicate-cube routes: the lint and lift findings that were in the log are now in the report. | Same. |
| **mununu-ui** | None: the UI does not read the N-source report. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the field. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the remedy-message audit of 2026-11-19 (mununu#1106 item 3): `to_pair` in `dispatch_adapter` discarded `AdapterOutput::warnings`; `dispatch_sv_predicate_cube` logged the lift's.
- **Tests:** `x1106_an_adapters_warning_about_a_dropped_variable_reaches_the_source_report` (the reproducer: an xstate source whose numeric context variable has no bounds; read through JSON so it compiles and fails on the parent, on the inspect and the verify paths).

## Not covered here

- The composition notes on the CTXDSL `context eval` path are a separate change (mununu#1106 item 5, its own briefing).
- The `c-codesign` route returns CTXDSL text only and has no warning channel yet; an empty list is reported there, which is the truth of what it carries, not of what it dropped.
