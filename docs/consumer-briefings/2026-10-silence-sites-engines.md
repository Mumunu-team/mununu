# Consumer briefing — a VIOLATED row without a path says why; the owned portfolio keeps every member's refusal; a malformed sidecar is one warning that names what is lost (2026-10)

> Source of truth: [`WitnessMiss`](../../crates/mununu-core/src/adapter/reach_rescue.rs) · [`decide_reach_owned_only`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) · [`build_cell_domains`](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs) — surface: (CLI+API) — `sv verify-auto` notes, `btor2 verify --owned-only` `refused`, the lift's warnings

**Audience:** rosf, monono and anyone who reads mununu's notes, warnings or the portfolio's `refused` list. Closes the HIGH and MEDIUM rows of #803 (`docs/policies/failure-paths.md` Rule 1, the engine modules' Class B audit).

## TL;DR

- **New note, no new flag:** a VIOLATED safety row that ships no `trace` now carries a `witness-unavailable` note (`level: info`, `property` set, `items: [stage]`) saying which stage of the path finder declined — `shape` (the formula has no one-path reading), `monitor`, `parse`, `search` (the finder's own error), `bound` (no path within the bound), `slice` (the rescue slice expired first) — and why. The verdict is unchanged; a VIOLATED without a path was always a VIOLATED.
- **`refused` is complete on the owned path too:** `btor2 verify --owned-only` (and every owned-portfolio run) now records the native BMC member's and the deep-cex member's refusals beside the verdict, as the full portfolio already did (#734). A design the encoder declines no longer reads as "native abstained".
- **A malformed sidecar is one warning that names everything it would have declared** (cell domains, the reset sequence, havoc / UF wraps, seeds, valuations); the memory-declaration check now reports the parse error instead of "every memory undeclared". Verdicts unchanged (a malformed sidecar was already ignored; it is now said).
- Removed: the undiagnosed `reach_portfolio_rescue` wrapper (library API; tests only used it). `bmc_cex_until` returns `Result` (library API).
- A set-but-unparseable `MUNUNU_BDD_MAX_BITS` / `MUNUNU_BDD_ARENA_NODES` now logs a warning instead of silently taking the default.
- JSON shape, schemas, flags, exit codes: unchanged (notes and `refused` are existing fields).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | a VIOLATED row without a trace carries a `witness-unavailable` note; `refused` may list `native` / `cex` where it was empty | none required; if you surface notes, this one is `info` |
| **monono** | same; a malformed `.mununu.json` now produces a warning whose text names what was lost, and a memory-check error that names the parse error | none required; read the warning if a sidecar stopped taking effect |
| **mununu-ui** | nothing (RTL verbs) | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the notes and refusals live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow needs the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | the gates below ran here |
| `mununu/docker/Dockerfile.profile`, `.extract*` | no | unaffected |
| rosf / monono images | behaviour updates on the next binary pull | — |

## Measured

| run | result |
|---|---|
| seam tests: a formula outside the witness shape is a `shape` miss, a short bound a `bound` miss, an out-of-range constant the finder's `search` refusal; the note carries kind / property / stage; the owned driver records the native member's refusal on an array state; a malformed sidecar is one warning naming what is lost | green (reach_rescue 46, native_bmc 21, reach_portfolio 14, witness 6, fault_adversary 4, trap_certificate 6; clippy clean on core + cli) |
| verdict-identity gate in `mununu-sva` (2026-10-26): `e2e_opentitan_sva_census` on this branch vs `main` bc82372 | **0 verdict moves** — 191 rows, 173 holds / 7 violated / 11 skipped / 0 ⊥ on both (2,483 s) |
| `diff_corpus_monotone_verdict_ledger` on this branch | PASS (44 s) |

## Provenance

- Branch `fix/silence-sites-803`; issue #803 (the audit); policy `docs/policies/failure-paths.md` Rule 1.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The trap certificate's and the fault adversary's missing path reaches the log (`tracing::warn!`), not their reports — those reports have no notes channel yet.
- The lift's lint entry points parse the sidecar on their own and still default silently on a malformed one; the `to_ir` / `translate` entry is covered by the warning.
- The fifteen `#[allow(clippy::unwrap_used)] // M1 pending` extractors (Class A, PR 3).
