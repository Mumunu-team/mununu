# Consumer briefing — resume `sv verify-auto` from a killed run's breadcrumb (roadmap J3, 2026-11)

> Source of truth: [`adapter::resume::parse_breadcrumb`](../../crates/mununu-core/src/adapter/resume.rs); the CLI flag `--resume-from` in [`crates/mununu-cli/src/main.rs`](../../crates/mununu-cli/src/main.rs), the request field `resume_from` and the view field `resumed_from` in [`crates/mununu-core/src/api/models.rs`](../../crates/mununu-core/src/api/models.rs) — surface: CLI+API (`sv verify-auto --resume-from FILE` and `POST /api/v1/sv/verify-auto`).

> **Audience:** rosf and monono (run the gate, parse the report), mununu-ui (shows rows), anyone whose CI step gets killed.
>
> **TL;DR — one new CLI flag, one new request field, one new per-property field, one new key in every breadcrumb record. No verdict changes for a run without the flag.** A run the OOM killer or a step timeout ended leaves its `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` breadcrumb; `--resume-from FILE` (API: `resume_from`, the file's content) reads it back and does not re-run the properties whose last record is a definite verdict **from the same build**. Those rows come back with `decided_by: "resumed"`, `resumed_from: "<build tag>"` and a `detail` naming the build; a breadcrumb from another build is refused (exit `1` / HTTP `400`). Every breadcrumb record now carries `"build"`.

## What changed

| surface | item | meaning |
|---|---|---|
| CLI | `sv verify-auto --resume-from FILE` | Read the breadcrumb; skip every property whose last record is `holds` / `violated`; carry that verdict. Read before the run opens its own breadcrumb, so `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` may name the same file. |
| API | `SvVerifyAutoRequest.resume_from: string?` | The breadcrumb's **content** (NDJSON), not a path. |
| report | `properties[].resumed_from: string?` | Present only on a carried row: the build tag (full commit sha, `-dirty` if the tree was not clean) of the binary whose breadcrumb decided it. Such a row has `decided_by: "resumed"`, `detail: "resumed from the breadcrumb of build <sha7>"`, and no `trace`, `vacuity`, `seeded_predicates` or `wall_ms`. |
| report | note `resumed-from-breadcrumb` | One per resumed run: the rows carried, the rows re-run because their records disagreed, the breadcrumb rows the design no longer has. |
| breadcrumb | `"build"` on every record | The writing binary's identity. Omitted (not `null`) when the binary registered none. |
| refusals | exit `1` / HTTP `400` | A breadcrumb from another build; a record without `build` (an older binary); a binary without a build identity (built without git and without `MUNUNU_BUILD_SHA`); an empty or malformed file. The message names the fix. |

**Not resumed:** a property whose records disagree (`holds` from one portfolio member, `violated` from another) is re-run — the merge would have raised its engine-contradiction alarm and a resume must not pick a side. A `violated` carried from the breadcrumb has `false_cells: 0` — the count did not travel; read `detail` / `resumed_from`, not the count.

## Per consumer

| consumer | what this means for you | action |
|---|---|---|
| **rosf** | A gate step that was killed can finish on the next run from the breadcrumb instead of starting over. One additive key per carried row; one additive key per breadcrumb record. A row with `resumed_from` has no trace: a step that renders counterexamples must treat it as "violated, trace in the previous run". | Optional: pass `--resume-from` on a retry when the previous step's breadcrumb exists. If the report model is strict, add `resumed_from` (optional string). A breadcrumb reader that keys on an exact field set must accept `build`. |
| **monono** | Same, through rosf. | None. |
| **mununu-ui** | A row may say it was resumed; show `resumed_from` as provenance and do not offer a trace for it. | Optional. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` / the release images / the on-prem bundle | Yes, to get the flag and the `build` key. Nothing breaks without it. | A CLI flag, a request field and a report-shape addition. |
| rosf `docker/Dockerfile*` | No. | Consumer of the report; additive fields. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | No. | Same. |

## Provenance

Roadmap J3 (delivery lane), 2026-11-08. The breadcrumb itself is mununu#504 C7; its last-record-per-property reading is mununu#733; the build identity is mununu#715. The reproducer tier is unit-level at the seam (`adapter::resume` parses; `apply_resume` overlays; the planner's `member_options` keeps the skip set on every member); the slang-gated end-to-end is `make e2e` territory and was not run on the laptop.

## Not covered here

The estimator (J2) and the scheduler (J5) that will read `wall_ms`; the per-run lift cost as a report field; a breadcrumb written by two different builds in one file (refused at the first foreign record, by design).

## Update (mununu#933, 2026-11-09) — a breadcrumb is tied to the run's inputs

Every breadcrumb record gains `"run": "<16 hex digits>"`, a stable hash of the run's inputs: sources (names and contents), top, lift options, and every verify option that can change a verdict. `--resume-from` / `resume_from` now also refuses a breadcrumb whose `run` differs from this run's, or whose records have no `run`. Exit `1` on the CLI and `400` on the API, as for another build. **This is a verdict-correctness fix:** before it, a breadcrumb from one design carried `holds` / `violated` onto every row with the same name in another design or an edited one.

| consumer | what this means for you | action |
|---|---|---|
| **rosf** | A breadcrumb written before this release has no `run` and is refused; resume only the exact command that was killed (row selection and `--resume-from` itself are outside the hash). | Re-run once without `--resume-from` after upgrading. Accept the new `run` key if your breadcrumb reader is strict. |
| **monono** | Same, through rosf. | None. |
| **mununu-ui** | Not affected. | None. |

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` / the release images / the on-prem bundle | Yes, for the fix. | Correctness. |
| rosf `docker/Dockerfile*` | No. | Consumer. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | No. | Consumer. |

