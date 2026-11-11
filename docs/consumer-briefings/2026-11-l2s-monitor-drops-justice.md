# Consumer briefing: rIC3 now takes part in liveness-to-safety (l2s) queries; some justice rows may decide (2026-11)

> Source of truth: [`emit_l2s_core`](../../crates/mununu-core/src/adapter/btor2/l2s_monitor.rs) — surface: CLI+API

**Audience:** anyone who runs justice / liveness properties through the l2s rung, from the CLI or the HTTP API:
- `btor2 verify-liveness --justice …` with `--rung l2s` or `both`;
- the liveness rescue behind `sv verify-auto`.

## TL;DR

- **rIC3 now actually runs on l2s queries.** The monitor the l2s rung builds no longer carries the design's own `justice` / `fair` lines. rIC3 1.5.2 crashed on a `justice` line before solving, so until now the rIC3 member contributed nothing to an l2s query on a justice design; its note read "ABORTED by signal 6" (#1004), or "exited with status signal: 6" before that.
- **Some `unknown` justice rows may now decide.** Example: HWMCC'17 LIVE `cugbakro` came back `holds` in 38 s in a standalone run on the box. The new verdicts come only from rIC3 deciding queries it used to crash on.
- **No existing definite verdict should change.** The question each solver answers is the same: the monitor's single `bad`, with the fairness already built in. None of the safety solvers needs the dropped lines to answer it.
- **Unchanged:** the JSON shape, the exact rung, the k-liveness rung (it already dropped these lines), and every non-justice property.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Only if you run justice / liveness rows through l2s: an `unknown` row may now come back definite, and its `decided_by` may name rIC3. | Re-run the affected rows. |
| **monono** | Same, for any liveness guarantee that reaches the l2s rescue. | Same. |
| **mununu-ui** | None. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes. | They ship the `mununu` binary. The rIC3 member exists only where rIC3 is installed, as in `mununu-sva`. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Origin:** mununu#980.
  - Found as a SIGABRT in the M3 / M3b HWMCC'17 LIVE runs.
  - Reproduced standalone on the box in `mununu-sva` (`cugbakro`, `cujc128ro`, rIC3 1.5.2).
  - Scope: 31 of 31 l2s notes naming rIC3 in m3c recorded the crash.
- **Test:** `the_monitor_drops_the_designs_justice_and_fair_lines`. It fails on the parent commit and passes on this one.

## Not covered here

- **How many rows now decide:** not yet measured. A re-run of the open LIVE rows on this build will say.
- **btormc and Pono:** whether they read the dropped `justice` line was not measured.
