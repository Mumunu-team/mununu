# Consumer briefing: messages speak in the user's names — no roadmap tags, tiers or Rust identifiers (2026-11)

> Source of truth: the message sites in [`adapter/btor2/bit_blast.rs`](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs), [`adapter/btor2/kmts_lift.rs`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs), [`adapter/btor2/cegar.rs`](../../crates/mununu-core/src/adapter/btor2/cegar.rs) and [`api/handlers.rs`](../../crates/mununu-core/src/api/handlers.rs) (the controllability-aware lift summary) — surface: CLI+API — adapter warnings, cegar warnings and notes, the import endpoint's lift summary, the cegar endpoint's engine warning, the sidecar-widening summary

**Audience:** anyone who matches on the text of an adapter warning, a cegar warning, the `/api/v1/context/import` lift summary or the sidecar-widening summary.

## TL;DR

- **Message text only (mununu#1121, U2.7).** About a hundred user-facing strings carried a roadmap or review tag (`§Phase 10 §10.2 stage 1b`, `R-S2b.6`, `R-F5`, `[R.2.5b-smt-must]`, `(B.4.a)`, `(A.4)`, `[R.6.7 V.6 …]`, `PO-3`, `Tier-2`), a Rust identifier (`SmtPerTarget`, `MustHyperOnly`, `KleeneBot`, `PredicateSource::CraigInterpolation`, `AdapterOptions::sidecar_path`, `codesign::compose`, `evaluate_tri`) or an internal plan name as if the reader could act on it. Each now says the thing in the user's words: the flag that selects it on the verb that has it (`--may-edge-inference smt-all-pairs` on `btor2 cegar` and `sv cegar`, `--sidecar`, `--engine exact-symbolic` on `sv verify-auto`), the sidecar key (`abstraction: havoc`), or the plain description ("undecided (⊥)", "hyper-must edges", "the eager lift strategy"); where a tag was the only statement of why, the owner clause of #1139 says it.
- **One remedy that could not be followed is gone:** the cegar warning that offered `CegarOptions::smart_uf_cap = false` (a Rust field no verb exposes) now says what the cap did, how many iterations the run will use, and that running past it has no switch on `btor2 cegar` or `sv cegar` today.
- **No verdict, kind, field or JSON shape change.** The `/api/v1/context/import` lift summary reads "controllability-aware lift: cubes=…, may-only=…, definite=…, hyper-must=…" instead of "[R.6.7 V.6 controllability-aware lift] cubes=…, mayonly=…, sharp=…, hyper_must=…"; the cegar endpoint's engine warning reads "engine=symbolic:" without "(R-F5)". A consumer that matched an old marker must update the match (mununu-ui mentions the old tags only in comments, not in matches).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Warning and note text reads differently; kinds unchanged. | Nothing, unless you matched an old tag (no match found in the repository). |
| **monono** | Same. | Same. |
| **mununu-ui** | The import lift summary and the cegar warning strings shown verbatim read differently; the doc comments in `endpoints.ts` that cite `R.6.7 / V.6` describe fields, not text, and still apply. | None required. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the text. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the remedy-message audit of 2026-11-19 (Track U2 row 7, mununu#1121; the per-site table is on the issue; the `smart_uf_cap` site was its one HIGH).
- **Tests:** `x1121_widening_summary_names_the_stage_without_a_roadmap_tag` (the reproducer: the sidecar-widening summary lines name the stage in words and carry no tag; compiles on the parent and fails there on the `R-S5:` prefix), and the existing marker tests updated to the new words.

## Not covered here

- The 139 messages under `adapter/` that open with a module path as a prefix (`adapter/btor2/bad_monitor: …`) keep that prefix; whether to drop it is a separate decision noted on the issue.
- Docs that quote old warning text as examples are not refreshed here.
