# `crewai_handoff` — verify-framework example for native CrewAI sources

> **Source of truth:** [`crates/mununu-core/src/adapter/crewai/`](../../../crates/mununu-core/src/adapter/crewai/) — surface: CLI+API+UI.

End-to-end demonstration that `mununu verify` accepts a CrewAI
`.crewai.json` source as one of its `[[sources]]` entries. The
[`CrewaiAdapter`](../../../crates/mununu-core/src/adapter/crewai/mod.rs)
translates the crew into per-agent automata (`Idle -> Executing -> Done`
on `agent_<role>_start` / `agent_<role>_complete`) plus a sequential
supervisor, all composed asynchronously per
[Doc C §C.5](../../../docs/archive/design/hw-sw-codesign-extraction.md).

## What it demonstrates

- **Native CrewAI dispatch.** `[[sources]] adapter = "crewai"` — no
  manual rewrite into XState. The adapter handles the `agents`, `tasks`,
  `process = "sequential"` shape directly.
- **Multi-automaton-per-source composition via wildcard.**
  `[composition].members = ["crew.*"]` expands to every automaton
  emitted by the CrewAI source: `Agent_Researcher`, `Agent_Writer`,
  and the sequential `ResearchAndWriteSupervisor`. The composition
  reports `members = [Agent_Researcher, Agent_Writer,
  ResearchAndWriteSupervisor]`. Plan Part 6 item 4.
- **Per-automaton properties via the `over` field.** Each property
  targets a specific emitted automaton — the agent-level `reachable`
  liveness checks run against `Agent_Researcher` and `Agent_Writer`
  independently; the pipeline-completion check runs against the
  supervisor. The CrewAI source's internal asynchronous composition
  is also preserved verbatim in the assembled CTXDSL for future
  composed-state property authoring.

## Files

| File | Purpose |
|---|---|
| `crew.crewai.json` | Two-agent sequential CrewAI crew (Researcher -> Writer) |
| `verify.toml` | Project config (single source + composition + 2 properties) |
| `validate.sh` | End-to-end reproduction script |
| `transcript.txt` | Byte-deterministic expected output |

## Reproduce

```bash
bash examples/verify/crewai_handoff/validate.sh
```

Re-running against the same commit must produce a byte-identical
`transcript.txt`.

## Run manually

```bash
mununu verify examples/verify/crewai_handoff/verify.toml
mununu verify examples/verify/crewai_handoff/verify.toml --json
mununu verify examples/verify/crewai_handoff/verify.toml --strict  # exits 0 here (both props satisfied)
```
