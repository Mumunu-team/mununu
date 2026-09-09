# Consumer briefing — the process-memory ceiling is now ON by default in containers

> **Audience:** anyone running `mununu sv verify-auto` in a memory-limited container — CI lanes
> especially. **ROSF, monono, mununu-ui.**
>
> **⚠️ This briefing SUPERSEDES a line in
> [`2026-09-process-memory-ceiling.md`](2026-09-process-memory-ceiling.md).** That briefing said
> *"**Default unset ⇒ disabled** — no behaviour change unless you opt in"* and *"Default behaviour —
> unset ⇒ disabled ⇒ current behaviour. Consumers opt in explicitly."* **Both statements are no
> longer true.** Unset now means *auto*. If you read that briefing and concluded no action was
> needed, read this one.

## TL;DR

`MUNUNU_MAX_PROCESS_MEMORY_BYTES` used to default to **disabled**. It now defaults to **80% of a
detected cgroup memory limit**. With no detected limit — an unconstrained developer machine — it
stays disabled and nothing changes.

Set `MUNUNU_MAX_PROCESS_MEMORY_BYTES=0` to opt out.

## The resolution table

| value | ceiling |
|---|---|
| a positive integer | that many bytes (explicit; **unchanged**) |
| `0` | **disabled** — the escape hatch |
| non-numeric | disabled, with a debug log (unchanged) |
| **unset** | **auto**: 80% of a detected cgroup limit, else disabled |

Detection reads cgroup v2 (`/sys/fs/cgroup/memory.max`) then v1
(`/sys/fs/cgroup/memory/memory.limit_in_bytes`). A `max` value, a v1 "unlimited" sentinel, an
unparseable value, or a missing file all count as **no limit**.

## Why the default moved

The ceiling converts an allocator `abort()` — exit 134, which kills every property in the
invocation and reports *none* — into per-property abstentions with a `memory-budget-exceeded`
note, preserving prior verdicts. Defaulting it off meant the protection was absent exactly where
it is needed most: a containerised CI lane, whose operator has no reason to know the variable
exists until a run has already crashed. That is the failure mode mununu#490 was filed for, and
opt-in defaults do not reach the people who hit it.

## What it costs — read this before adopting

**An auto ceiling can abstain on a run that would have finished.** RSS at 80% of the container
limit does not guarantee an OOM; a process can sit there and complete. Where that happens you now
get `unknown` instead of a verdict.

This matters because **`ci_exit_code` fails on `unknown` but never on `skipped`.** A tightly-sized
container could turn a previously-green lane red.

Three things bound the cost, and one of them is yours to use:

1. The ceiling engages **only when a container limit is actually detected**. Unconstrained hosts
   are unaffected.
2. 80% is deliberately loose, not a tight fit.
3. **`=0` disables it outright.**

## Per-consumer

### ROSF / monono (CI lanes — the affected audience)

- **What to update:** decide, per lane, whether you want the ceiling.
  - **Keep it** (recommended for lanes that have ever been OOM-killed): no action. A run that
    would have crashed now degrades to per-property `unknown` with prior verdicts preserved.
  - **Opt out** (for lanes sized close to their limit where any abstention fails the gate):
    set `MUNUNU_MAX_PROCESS_MEMORY_BYTES=0`.
  - **Tune it:** set an explicit byte count; an explicit value always wins over auto.
- **What to expect:** in a container, a run approaching its memory limit produces `unknown`
  verdicts with a `memory-budget-exceeded` note rather than exit 134.
- **Report parsing:** no shape change. The `memory-budget-exceeded` note text is unchanged from
  #490.

### mununu-ui

- **What to update:** nothing. No wire-format change.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | Rust only; no tool pins touched | No |
| `mununu-sva` | Inherits `mununu-dev`; tool pins unchanged | No |
| `mununu-sva-pono` | Inherits `mununu-sva`; tool pins unchanged | No |
| `hw-verif` | Not involved | No |

Note the images themselves need no rebuild, but **running them under `--memory` now activates the
ceiling** where it previously did not. That is the behaviour change, and it needs no rebuild to
take effect.

## Test the transition

Detection was verified against a real limited container, not just unit-tested:

```
$ docker run --rm --memory=2g mununu-dev cat /sys/fs/cgroup/memory.max
2147483648
```

⇒ auto ceiling = 0.8 × 2 GiB = 1.6 GiB.

The resolution table itself is covered by pure unit tests that take the raw env value and the
detected limit as arguments, so they need neither env mutation nor a cgroup:
`explicit_env_value_wins_over_a_detected_limit`,
`explicit_zero_disables_even_when_a_limit_is_detected`,
`unset_with_a_detected_limit_auto_derives_the_ceiling`,
`unset_with_no_detected_limit_stays_disabled`,
`a_non_numeric_value_disables_rather_than_auto_detecting`, and
`the_auto_ceiling_never_exceeds_the_detected_limit` (the invariant that keeps the mechanism from
being inert — a ceiling above the real limit could never fire before the OOM killer).

To check what your own lane will do:

```bash
cat /sys/fs/cgroup/memory.max 2>/dev/null || cat /sys/fs/cgroup/memory/memory.limit_in_bytes
# a number  -> auto ceiling = 80% of it
# "max" / no file -> no ceiling, unchanged behaviour
```

## Provenance

- Issue: mununu#504 (C6), following mununu#490 which introduced the ceiling itself.
- Fix: `adapter::memory_budget::resolve_memory_budget` +
  `adapter::memory_budget::detect_cgroup_memory_limit_bytes`.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- **The coarse-granularity limit is unchanged.** The check fires between properties and at each
  `escalate_bottom` step; it cannot catch an allocation that fails within a single BDD blast
  between checkpoints. This is a graceful-degradation lever, not a crash guarantee.
- **Non-cgroup limits are not detected.** `ulimit -m` / `ulimit -v` and macOS memory pressure are
  not read; set the variable explicitly there.
