# Consumer briefing — recovering verdicts from a run that gets killed

> **Audience:** anyone whose `sv verify-auto` lane has ever been killed mid-run — OOM killer, CI
> step timeout, `docker stop`. **monono especially: this is the fix for the run you lost.**

## TL;DR

New opt-in env var. Point `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` at a path and `sv verify-auto` appends
one JSON line per property as it completes, flushing every write. When the process is killed, the
file holds every verdict it had already computed.

```bash
MUNUNU_VERIFY_AUTO_PARTIAL_JSON=/tmp/verdicts.ndjson mununu sv verify-auto design.sv
```

Unset ⇒ no breadcrumb. **No behaviour change unless you opt in.**

## Why this exists, specifically

`MUNUNU_MAX_PROCESS_MEMORY_BYTES` (see
[`2026-09-memory-ceiling-auto-default.md`](2026-09-memory-ceiling-auto-default.md)) degrades
gracefully when mununu can observe its own trouble — it polls its own RSS and abstains.

It is powerless against a kill from **outside**. `SIGKILL` runs no Rust code: no destructor, no
panic handler, no final report. A lane that verified 24 of 25 properties reports nothing, and the
next run starts from zero. That is not hypothetical — it is the failure this repo's own source
comment records: *"monono lost a gate run to a process kill that emitted zero bytes, including for
properties mununu had already decided."*

The two levers are complementary: the ceiling prevents the death it can see coming, the breadcrumb
survives the one it cannot.

## The format

Newline-delimited JSON, one object per record:

```json
{"index":0,"property":"fifo_sva_0","outcome":"holds","phase":"main"}
{"index":1,"property":"fifo_sva_1","outcome":"unknown","phase":"main"}
{"index":1,"property":"fifo_sva_1","outcome":"holds","phase":"escalated"}
```

| field | meaning |
|---|---|
| `index` | the property's position in the report (not a write counter) |
| `property` | the assertion name |
| `outcome` | `holds` / `violated` / `unknown` / `skipped` — the same vocabulary as the report |
| `phase` | `main` (the per-property loop) or `escalated` (the ⊥ re-plan pass) |
| `elapsed_ms` | present only when timing was recorded; **an absent timing is an absent key**, not `null` |

### The one rule: read the LAST record per property name

A verdict can change after the main loop. The ⊥ re-plan / escalation pass runs afterwards and can
turn an `unknown` into a definite verdict; when it does, a second record is appended with
`"phase":"escalated"`. Only properties that actually moved get a second record.

Taking the last occurrence per `property` gives the verdict the final report would have carried.

```bash
# last-wins per property
jq -s 'group_by(.property) | map(last)' /tmp/verdicts.ndjson
```

## Guarantees, and their limits

**What holds:** every record on disk is a complete, valid JSON object on its own line. Records are
flushed individually, so the file is correct-as-of-the-last-flush at every instant — including the
instant a `SIGKILL` lands. Property names are serialized with `serde_json`, so a name containing a
quote, a backslash, or a newline cannot break the framing or forge a field.

**What does not:** the verdict for the property being worked on *when* the kill lands is not
there — it was never computed. A property's record is written at the start of the following
iteration, so the residual gap is loop bookkeeping only, not verification work.

**It is a diagnostic, never a gate.** An unwritable path, a full disk, or any write failure is
swallowed after a single warning and the run continues. A breadcrumb must not be able to fail a
verification run — that would trade a recoverable crash for an unconditional one.

**It is not a substitute for the report.** Verdicts only: no counterexamples, no verification
notes, no seeded predicates. A run that finishes normally still emits the full report, and you
should prefer it. This is for the run that does not get to finish.

## Per-consumer

### monono (the reporter of the original failure)

- **What to update:** set `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` in the gate lane, and on a non-zero
  exit read the breadcrumb before deciding the run produced nothing.
- **What to expect:** after a kill, a file with every completed verdict rather than zero bytes.
- **Suggested gate logic:** on a crash exit code, parse the breadcrumb last-wins; report the
  recovered verdicts as *partial* and the rest as not-run. Do not treat a recovered set as a
  complete run — the absence of a property means "never decided", not "passed".

### ROSF

- Same mechanism if you run `sv verify-auto` in a constrained container. The variable is read
  inside `verify_auto`, so it applies to the **API server** too — a server OOM-killed mid-request
  otherwise returns nothing for properties it had already decided.

### mununu-ui

- **What to update:** nothing. No wire-format change; the breadcrumb is a side file, not part of
  any HTTP response.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | Rust only; no tool pins touched | No |
| `mununu-sva` | Inherits `mununu-dev`; tool pins unchanged | No |
| `mununu-sva-pono` | Inherits `mununu-sva`; tool pins unchanged | No |
| `hw-verif` | Not involved | No |

## Test the transition

```bash
MUNUNU_VERIFY_AUTO_PARTIAL_JSON=/tmp/b.ndjson mununu sv verify-auto design.sv
wc -l /tmp/b.ndjson          # one line per property (plus escalation movers)
jq -c . /tmp/b.ndjson        # every line parses
```

Covered by `e2e_partial_json_breadcrumb_records_every_verdict`, which runs the full pipeline and
asserts the last record per property matches the final report verdict — the wiring is the part
that could silently do nothing, and a breadcrumb that is never written looks identical to a
correct one until the day you need it. Format and cursor behaviour are unit-tested separately,
including that a hostile property name cannot forge or split a record.

## Provenance

- Issue: mununu#504 (C7), complementing #490 / #504 C6.
- Implementation: `adapter::partial_json`, wired in `adapter::slang::verify_auto`.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- **No resume.** The breadcrumb lets you *read* what finished; it does not let a subsequent run
  skip those properties. Re-running re-verifies everything.
- **The file is truncated on open**, so a re-run does not append to the previous run's records.
  Copy it aside if you need the history.
- **One path, one run.** A process that calls `verify_auto` more than once — the **API server**
  across requests, or `sv mutate`'s baseline-then-mutant pair — reuses the same path, and the last
  opener wins. Point the variable at a distinct path per run if you need them separately. For a
  long-lived API server this means the breadcrumb reflects whichever request opened it most
  recently, which is useful for a single-tenant crash post-mortem and misleading for anything else.
