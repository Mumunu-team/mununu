# Consumer briefing: a model with no `bad` line is no longer `holds` (2026-11)

> Source of truth: [`no_obligation`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) — surface: CLI+API — `mununu btor2 verify` / `POST /api/v1/btor2/verify`. The `sv verify-bad` half (`require_bad_line` in `adapter/sv_verify.rs`) was removed with the verb in 4b447181 (mununu#1136): see [the removal briefing](2026-11-sv-verify-bad-removed.md).

**Audience:** anyone who reads the `outcome` of `sv verify-bad` or `btor2 verify`.

## TL;DR

- **A lifted design or BTOR2 file with no `bad` line used to report `holds` (mununu#1064).** A proof engine handed an empty obligation set answers "unreachable", and that reached the report as a proof.
- **`sv verify-bad` now fails with an error** naming the cause: the lifted design has no `bad` line. The lift drops immediate assertions and does not lower SVA, so a design whose checks are those lifts to exactly this. Use `sv verify-auto` for the SVA property set.
- **`btor2 verify` now reports `unknown`** on a file with no `bad` line, with the reason under member `portfolio` in `refused`.
- **No change** for a model with at least one `bad` line.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A `sv verify-bad` call on a design with no lowered `bad` exits non-zero with an error instead of `holds`; a `btor2 verify` on a `bad`-less file is `unknown`. | If a gate relied on such a `holds`, it was checking nothing: move the checks to `sv verify-auto`. |
| **monono** | Same. | Same. |
| **mununu-ui** | None: it does not call these verbs. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the change. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** an internal reproduction run, 2026-11-17 (#1064).
- **Tests:** `a_model_with_no_bad_line_is_unknown_with_a_reason_never_holds` (both portfolio drivers) and `verify_bad_refuses_a_lifted_model_with_no_bad_line`.

## Not covered here

- `sv verify-bad` has had no `bad` line to decide on most designs since the slang-only lift, which reads every design with `--ignore-assertions`. Whether the verb keeps a role is open on #1074.
- `sv verify-auto` already reports the immediate assertions the lift dropped (mununu#988); that is unchanged.
