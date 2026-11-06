# Consumer briefing — a VIOLATED row that ships no path now says why, and names the engine that decided it (2026-11)

> Source of truth: [`attach_safety_witnesses`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) + the exact member's fallback in [`verify_auto_impl`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: CLI+API — `properties[].decided_by` and the `notes` of `sv verify-auto` (`--json`, `POST /api/v1/sv/verify-auto`; no UI affordance).

**Audience:** rosf, monono and anyone who reads a VIOLATED row's `trace`, its `decided_by`, or the report's `notes`.

## TL;DR

- **No verdict changes and no JSON shape change.** Two things a consumer reads change on VIOLATED rows that ship no path.
- **`decided_by` is honest after an exact refusal.** When the exact engine refuses a recoverability row (`AG EF p`) at its bit cap, the scalable recoverability engine decides it. The row used to read `decided_by: "exact-symbolic"` (the portfolio's member label); it now reads `"explicit cube + smt-hyper-must (recoverability rescue)"`, the label the recoverability rescue lane already uses for the same engine.
- **The missing path is explained.** Such a row now carries a `witness-unavailable` note (`items: ["exact-refused"]`) whose text quotes the exact refusal. Any other VIOLATED non-safety row (recoverability, liveness) that reaches the end of the run with no path and no recorded reason carries one too (`items: ["no-path"]`), naming the engine that decided it.
- **No new note kind:** `witness-unavailable` (mununu#808) fires on more paths.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | a VIOLATED `AG EF` row decided after an exact bit-cap refusal reads the recoverability engine's label, not `exact-symbolic`; VIOLATED non-safety rows with no path carry a `witness-unavailable` note | if you match `decided_by == "exact-symbolic"`, the rows above no longer match; if you treat "VIOLATED without a path" as a defect to report upstream, read the note first |
| **monono** | same as rosf for any `sv verify-auto` row it reads | same as rosf |
| **mununu-ui** | none — no UI affordance for these fields | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag and read `decided_by` or `notes` | report-field change in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow reads `decided_by` or `notes` | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes if you run `sv verify-auto` in the image | the change lives in the binary the volume carries |
| `mununu/docker/Dockerfile.profile` | no | profiling image, report-independent |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | yes if rosf pins a mununu version and reads these fields | picks up the change |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | yes if monono pins a mununu version and reads these fields | picks up the change |

## Provenance

mununu#849: the OpenTitan `edn` IP's two endpoint `AG EF Idle` rows under the slang-only lift shipped VIOLATED, `decided_by: exact-symbolic`, no path, no note. Measured on the SVA host: the exact member runs with the cap retry off, refuses the rows' 405-bit cone at the 192-bit cap before any witness is attempted, and its error arm hands the row to the scalable recoverability engine, which decides VIOLATED and builds no path. Reproducer: `crates/mununu-core/tests/exact_bitcap_fallback_849.rs` — a 200-bit cone `AG EF` row that the fallback decides; on the parent it reads `exact-symbolic` with no note. It needs slang, so it runs in the `mununu-sva` image. The two seam tests beside `attach_safety_witnesses` cover the end-of-run note in CI.

## Not covered here

These rows still ship no path. The trap certificate builds one for the same shape when it is reached (it runs on ⊥ rows only, and these rows are no longer ⊥ once the fallback decides). Running it for a fallback VIOLATED is a separate change. The exact member's cap retry stays off on the first pass, as the planner intends.
