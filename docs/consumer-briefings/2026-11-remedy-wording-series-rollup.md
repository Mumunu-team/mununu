# Consumer briefing: the remedy-wording series — refusal text that changed without its own briefing (2026-11)

> Source of truth: [`docs/policies/failure-paths.md`](../policies/failure-paths.md) Rule 5 and the PRs named below — surface: CLI+API — the text of refusals and abstentions on the AIGER, TLSF, XState and yosys adapters, the native BMC / reach-portfolio budget stops, and the exact engine

**Audience:** anyone who matches on the text of a refusal, an adapter warning or a budget-stop note.

## TL;DR

- **Message text only.** Six PRs of the mununu#1106 / mununu#1121 wording series changed user-facing refusal text and shipped no briefing of their own, because none changes a verdict, a kind, a field or a JSON shape. This page lists them so a consumer that matches on one of those strings has one place to look:

| PR | surface | what the text says now |
|---|---|---|
| #1108 | AIGER (`mununu verify` / `translate`, `POST /api/v1/translate` with `format: aiger`) | a blank first line is refused by name instead of a panic; a binary `.aig` header names the route that decodes it |
| #1109 | TLSF (same verbs with `format: tlsf`, the synthesis verbs) | an unknown `MAIN` section is refused by name, saying the sections after it would not be read, with the remedy per kind of section |
| #1110 | XState (`mununu verify`, `POST /api/v1/verify`) | an unbounded numeric variable's warning names the `"__mununu": { "bounds": … }` key that bounds it and says what skipping does to guards that read the variable (the flag it used to name did not exist) |
| #1118 | `sv verify-auto` notes, the native BMC member's budget stop, the reach portfolio's wall | the knob named is the one of the verb the user ran (`MUNUNU_NATIVE_QUERY_RLIMIT` / the run budget on `sv verify-auto`; `--timeout-ms` on the `btor2` verbs only) |
| #1120 | TLSF | a `GLOBAL` section that declares anything is refused by name with the remedy (unfold to the basic format) instead of a bare lexer error |
| #1123 | every verb that runs yosys | one minimum yosys version (0.67) in the not-found message and the `read_slang` hint; the not-found message says what the version is for and names `MUNUNU_YOSYS_SLANG_PLUGIN` for an older yosys |
| U2.11 (this PR) | the yosys clock-domain refusals (every verb that lifts SystemVerilog); the memory-`init` remedy of the predicate-cube lift (`btor2 cegar`, `sv cegar`, `sv verify-auto`) | `--merge-clock-domains` is named as `sv verify-auto`'s flag and the other verbs are told they have no such switch; `--engine exact-symbolic` is named as `sv verify-auto`'s value, and "supply an `init`" became "an initializer in the RTL, or the `init` line of a hand-written BTOR2" |

- **No verdict, kind, `determinism`, count or shape change in any of them.** A consumer that matched an old sentence must update the match; the kinds and tags it branches on are unchanged.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Refusal text on the listed surfaces reads differently. | Nothing, unless you matched one of the old sentences (no match found in the repository). |
| **monono** | Same. | Same. |
| **mununu-ui** | Error strings shown verbatim read differently. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the text. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the laptop's cadence review of 2026-11-20, which listed the six PRs as text changes without a briefing while their siblings (#1111 #1112 #1113 #1117 #1119 #1122 #1124) shipped one.
- **Tests:** each PR's own reproducer (named in its `Repro:` trailer); for U2.11, `policy::remedies_on_the_verb_1121` — the per-verb check that fails on the parent on the two sites above.

## Not covered here

- The remaining rows of mununu#1121 (U2.2, U2.3, U2.7–U2.11) ship their own briefings.
