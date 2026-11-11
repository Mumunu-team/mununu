# Consumer briefing: a solver member killed by a signal is reported as an abort, not a refusal (2026-11)

> Source of truth: [`no_verdict_line`](../../crates/mununu-core/src/adapter/mod.rs) — surface: CLI+API

**Audience:** anyone who reads the portfolio notes or member refusal reasons in `sv verify-auto`, `btor2 verify*` or `btor2 verify-liveness` output, from the CLI or the HTTP API. That includes a consumer that greps those strings.

## TL;DR

- **The no-verdict line now names a signal death.** When the rIC3, btormc or Pono member dies from a signal and leaves no verdict, the line reads:
  ```
  `rIC3` ABORTED by signal 6 (SIGABRT) with no verdict — a crash inside the tool, not a refusal of the design: <reason>
  ```
  Before, it read `` `rIC3` exited with status signal: 6 … and no verdict: <reason> `` (the form #980 quotes). The reading this replaces is "the member refused the design". The signals named are SIGABRT, SIGSEGV, SIGBUS, SIGFPE, SIGILL and SIGKILL; any other signal shows its number only.
- **An ordinary non-zero exit is unchanged:** `` `X` exited with status … and no verdict: … ``.
- **Unchanged:** every verdict, and the JSON shape. The line still lands where it did, in the member's refusal entry and the portfolio note. Only its wording changes.
- **Unix only:** on Windows a process has no signal, so the plain form is kept.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Only if you match the refusal text: a crashed member no longer matches `exited with status signal`. | Match `ABORTED by signal` to count crashes separately from refusals. |
| **monono** | Same. | Same. |
| **mununu-ui** | The text it displays changes for a crashed member. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the wording. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Origin:** mununu#980. On HWMCC'17 LIVE instances, the l2s rung's rIC3 member died with SIGABRT on 5 of 5 observed reports, and the note read it as a refusal.
- **Test:** `a_signal_death_is_named_an_abort_and_an_exit_code_is_not`.

## Not covered here

- **Why rIC3 aborts on the l2s fair-cycle monitor:** it has not been reproduced standalone, and it needs the HWMCC'17 LIVE files and the `mununu-sva` image. It stays open on #980.
