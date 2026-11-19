# Consumer briefing: the init-less-register abstentions name the registers and a remedy the verb has (2026-11)

> Source of truth: [`BottomReason::UnestablishedInitialState`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) and [`BottomReason::UnestablishedInitialCubes`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) (`one_line`) — surface: CLI+API — the `bottom_reason.detail` of `mununu sv verify-auto` / `POST /api/v1/sv/verify-auto` for the kinds `unestablished-initial-state` and `unestablished-initial-cubes`

**Audience:** anyone who reads the text of a withheld or undecided row over a register with no established cycle-0 value.

## TL;DR

- **Message text only (mununu#1121, U2.1).** Both details used to end with "supply an `init` via a sidecar": `sv verify-auto` takes no sidecar and no key of any sidecar is called `init`, so the sentence named a fix that does not exist (`docs/policies/failure-paths.md` Rule 5). Each detail now **names the registers** (by the design's names, the first twelve then a count) and gives the remedy that exists: give them a reset value in the RTL and keep reset-gating on (`--no-gate-reset` / API `gate_reset: false` turns it off), and says that `sv verify-auto` cannot take an initial value from the command line or the request today — a mununu limitation, not something the design or the call can change. `docs/verifying-rtl.md` no longer offers `--pin` for a register (it ties a primary input).
- **No verdict, kind, `determinism`, `registers` count or shape change.** The rows are withheld or undecided exactly as before (`unknown`, the same kind, `determinism: reproducible`, `registers` = the same count); only `detail` reads differently. A consumer that matched the old sentence must update the match.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | The `detail` text of the two kinds changes; the kind and the `registers` count do not. | Nothing, unless you matched the old sentence (no match found in the repository). |
| **monono** | Same. | Same. |
| **mununu-ui** | None (the UI shows `detail` verbatim). | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the text. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the remedy-message audit of 2026-11-19 (Track U2 row 1, mununu#1121).
- **Tests:** `x1121_the_withheld_violation_names_the_registers_and_a_remedy_this_verb_has` (the reproducer: a named init-less state through the real withholding path; compiles on the parent and fails there on the old sentence), `x1121_the_initial_cube_abstention_names_its_registers_and_caps_the_list` (the seam: the cube path's text and the twelve-name cap).

## Not covered here

- Whether `sv verify-auto` gets a way to take an initial value (a flag and a request field) is the founder's decision (Track U3.1); until it does, the message says it cannot.
