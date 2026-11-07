# Consumer briefing — rows name their portfolio members; refusals reach the report (2026-11)

> Source of truth: [`PortfolioAttributionView`](../../crates/mununu-core/src/api/models.rs#L3028) + [`MemberRefusalView`](../../crates/mununu-core/src/api/models.rs#L3038) — surface: CLI+API — `sv verify-auto --json` / `POST /api/v1/sv/verify-auto` (`properties[].portfolio`), `btor2 verify` / `POST /api/v1/btor2/verify` / `POST /api/v1/sv/verify-bad` (`refused`); no UI affordance.

**Audience:** rosf, monono, mununu-ui, and anyone who reads which engine decided a row.

## TL;DR

- **Additive JSON fields only. No verdict changes.**
- **`sv verify-auto`: new per-row `portfolio` object.**
  - It appears when a reachability-portfolio rescue ran on the row (safety, liveness, fair-cycle or reachability lane). It has two fields:
    - `decided_by`: the engines that answered (`native`, `btormc`, `ric3`, …).
    - `refused`: `[{member, reason}]`.
  - The row's top-level `decided_by` is unchanged; it still names the stage (`reach-rescue`).
  - Before this change, the engines appeared only in the `portfolio-rescue` note's prose, and refusals only on a row nothing decided.
- **`btor2 verify` and `sv verify-bad`: new `refused` list**, beside `reachable_by` / `unreachable_by`. It is omitted when empty.
- **`btor2 verify --owned-only` now records the exact engine's refusal** (an over-cap cone, for example) instead of dropping it.
- **Subprocess refusals carry the tool's own reason.**
  - When pono, rIC3 or btormc exits with no verdict and an empty stderr, the reason is now taken from stdout, capped at 400 characters.
  - Example: pono's `Unsupported variable sort in IC3Bits: …` on array state used to read as an empty `stderr:`.
  - The message wording changes from `…and no verdict; stderr: <text>` to `…and no verdict: <text>`.
- **`portfolio-rescue`, `reachability-rescue`, `liveness-rescue` and `fair-cycle-rescue` notes now set `property`** to the row's name. Previously it was `null`.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **monono** | `properties[].portfolio` appears on rescued rows. A gate keyed on the top-level `decided_by` is unaffected. | Optional: read `portfolio.decided_by` instead of parsing the `portfolio-rescue` note. Re-pin any test that string-matches `no verdict; stderr:`. |
| **rosf** | Same new fields. `refused` appears on `btor2 verify` output when a member refuses. | None required. A strict-schema decoder that rejects unknown keys must allow `portfolio` and `refused`. |
| **mununu-ui** | No UI affordance. A TypeScript type mirroring `PropertyVerdictView` or `Btor2VerifyResponse` gains two optional fields. | Optional type update. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | Yes, if consumers pin a version tag and want the fields. | The fields come from the binary. |
| `mununu/docker/Dockerfile.dev` | Only if the dev workflow reads the fields. | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | Yes, if you run `sv verify-auto` / `btor2 verify` in the image. | The binary on the volume changes. The image's toolchain does not. |
| `mununu/docker/Dockerfile.profile` | No. | Profiling image. |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | No. | Extraction images do not verify. |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | Yes, if rosf pins a mununu version and wants the fields. | Picks up the change. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Yes, if monono pins a mununu version and wants the fields. | Picks up the change. |

## Provenance

- **mununu#873.** Lever measurements need per-row attribution:
  - the N5 before/after on edn, and the N9 gate "can the owned engines replace btormc" (24 rows);
  - the HWMCC'20 array track, 315 instances, where native's `ArraySortUnsupportedInBvOnly` refusal on every instance was only visible in debug logs.
- **Tests.**
  - `a_rescued_row_names_its_deciding_and_refusing_members`: a 256-bit register held at zero is decided by native and refused by exact over the cap, on a decided row.
  - `subprocess_failure_reason` unit tests: stdout fallback, cap, and the empty case.
  - The `api_schema_drift_*` schemas are regenerated.

## Not covered here

- **Per-member wall time.** It would make the report host-dependent; that needs its own decision.
- **The members that abstained** are not listed by name. They are the portfolio's members minus the two lists.
- **cvc5's subprocess error message** (`adapter/cvc5`) still quotes stderr alone. cvc5 is not a reach-portfolio member, so its message reaches the interpolation engine's diagnosis, not `refused`.
- **The recoverability rescue (`AG EF`)** names its rung in `decided_by` as before. This change does not attach a `portfolio` object to it, including the trap certificate's safety queries.
