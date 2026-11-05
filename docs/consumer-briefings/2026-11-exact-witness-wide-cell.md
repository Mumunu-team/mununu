# Consumer briefing — a VIOLATED liveness / recoverability row that ships no path now says why (2026-11)

> Source of truth: [`BddBitBlaster::refuse_wide_cells`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) + [`ExactSession::attempt`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) — surface: CLI+API — the `notes` of `sv verify-auto` / `btor2 verify` reports (`--json`, `POST /api/v1/sv/verify-auto`; no UI affordance).

**Audience:** rosf, monono and anyone who reads a VIOLATED row's `counterexample` and the report's `notes`.

## TL;DR

- **A report-silence fix; no verdict changes, no JSON shape change.** When the exact engine refutes an `AF p`, `AG AF p` or `AG EF p` row, it tries to build the concrete path. Two cases used to drop that path **without a word in the report** (only a log line): the property's cone holds a cell wider than 128 bits (the path carries each cell as a 128-bit value), or the detected shape's builder found no path. The row then shipped **VIOLATED with `counterexample: null` and no reason**.
- **Now** both cases add the existing **`witness-unavailable`** note to the row (`items: ["exact-witness"]`), whose text names the cell and its width (`no witness: cell `<name>` is <n> bits wide …`) or says the refutation could not be reconstructed as one path. The verdict is unchanged; only the explanation is new.
- **No new note kind and no shape change:** `witness-unavailable` already exists (mununu#808); it now fires on two more paths.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | a VIOLATED `AF` / `AG EF` row with no path may now carry a `witness-unavailable` note naming why; the verdict and every other field are unchanged | none; if you treat "VIOLATED without a path" as a defect to report upstream, the note is now the reason to read first |
| **monono** | same as rosf for any `sv verify-auto` / `btor2 verify` row it reads | none |
| **mununu-ui** | none — no UI affordance for these notes | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag and read `notes` | report-field fix in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow reads `notes` | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes if you run `sv verify-auto` / `btor2 verify` in the image | the fix lives in the binary the volume carries |
| `mununu/docker/Dockerfile.profile` | no | profiling image, report-independent |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | yes if rosf pins a mununu version and reads `notes` | picks up the fix |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | yes if monono pins a mununu version and reads `notes` | picks up the fix |

## Provenance

Seen on the OpenTitan `edn` IP's two endpoint `AG EF Idle` rows under the slang-only lift (mununu#849): VIOLATED, no path, no note. Measured on the SVA host, these rows reach **neither** branch fixed here. The exact engine refuses their 405-bit cone at the bit cap before any witness is attempted, and a fallback engine decides the row; that path is mununu#849's own fix, a separate PR. The two branches fixed here were silent in the same way on other rows, which is why they are fixed on their own. Reproducer: `crates/mununu-core/tests/exact_witness_wide_cell_849.rs` — an `AG EF` and an `AF` row over a design with a 130-bit cell in the cone; on the parent both ship VIOLATED with no recorded reason, on the fix the reason names the cell and its width.

## Not covered here

Carrying cells wider than 128 bits in the path itself (so these rows ship a path, not a reason) is not done; the trace already carries wide values as hex (mununu#723), the exact engine's path builder does not. A formula with no one-path counterexample shape still ships no path and no note from this site; its witness, when one exists, comes from the safety path.
