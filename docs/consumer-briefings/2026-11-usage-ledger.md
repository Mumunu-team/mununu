# Consumer briefing — the opt-in local usage ledger (roadmap H3, 2026-11)

> Source of truth: [`usage_ledger::record`](../../crates/mununu-core/src/usage_ledger.rs) and [`UsageRecord`](../../crates/mununu-core/src/usage_ledger.rs); the `ledger` field of [`licence::LicenceStatus`](../../crates/mununu-core/src/licence.rs); `[licensing] ledger` in [`deployment`](../../crates/mununu-core/src/deployment.rs) — surface: CLI+API (`sv verify-auto` on both; `mununu licence status`, `GET /api/v1/licence`).

> **Audience:** rosf and monono (run the gate in CI), mununu-ui (shows `licence status`), anyone operating a self-hosted mununu.
>
> **TL;DR — nothing changes unless you turn it on.** A deployment may name a local NDJSON file (`[licensing] ledger = "…"` in `mununu.toml`, or `MUNUNU_USAGE_LEDGER`); every `sv verify-auto` run on the CLI or the API then appends one record — time, verb, surface, version, build, licence state / tier / team, result, verdict counts, wall. **No design content** (no path, file, module or property name, formula, trace or note): the record type is a closed field set pinned by a test. `GET /api/v1/licence` and `mununu licence status` gain an optional `ledger` (the path). No verdict, exit code or report shape changes.

## What changed

| surface | item | meaning |
|---|---|---|
| config | `[licensing] ledger = "path"` in `mununu.toml`; env `MUNUNU_USAGE_LEDGER` | Names the ledger; relative paths resolve against the toml. Unset = off. |
| CLI + API | one record per `sv verify-auto` run | Appended after the run, before the CI gate exits, on success **and** on a run that failed before a report (`result: "error"`, counts zero). |
| CLI + API | `licence status` / `GET /api/v1/licence` → `ledger: string?` | The path when on; absent when off. `licence-status.schema.json` regenerated (additive). |
| failure path | an unwritable ledger | CLI: one `warning:` line on stderr; API: a `warn` in the server log. The run is unaffected; the record is lost and the message says so. |
| bundle | `deploy/mununu.toml`, `deploy/compose.yaml` | Commented-out `ledger =` and `./ledger:/var/lib/mununu` volume; `deploy/README.md` says how. |

The record (`RECORD_KEYS`, in order): `ts, verb, surface, version, build?, licence_state, licence_tier?, licence_team?, result, properties, holds, violated, unknown, skipped, unsupported, wall_ms`.

## Per consumer

| consumer | what this means for you | action |
|---|---|---|
| **rosf** | Off by default; your CI output and the report are unchanged. If an operator turns it on in a CI image, the ledger file must be on a writable path or every run prints one stderr warning (exit codes unchanged). | None. Optional: mount a ledger path in long-lived runners. |
| **monono** | Same, through rosf. | None. |
| **mununu-ui** | `GET /api/v1/licence` may carry `ledger`; show it on the licence panel if you render the status object. | Optional, additive. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` / the release images / the on-prem bundle | Yes, to get the feature; nothing breaks without it. | A config key, a status field, a writer. |
| rosf `docker/Dockerfile*` | No. | Behaviour unchanged unless configured. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | No. | Same. |

## Provenance

Roadmap H3 (delivery lane, Track H — on-prem licences), 2026-11-08. The two falsifiers the roadmap set are tests at the seam: `grep -c` over the file is the run count (`append_writes_one_line_per_record_and_grep_count_is_the_run_count`), and a record built from a report full of design names carries none of them (`a_record_carries_counts_and_identity_and_no_design_content`, `the_key_set_is_exactly_the_allowlist`).

## Not covered here

Other verbs (`sv mutate`, `btor2 verify*`) write no record yet — the `verb` field is where they would appear. No aggregation or export tool: the file is plain NDJSON and `grep`/`jq` read it (examples in `docs/licensing.md`). The cost estimator (J2) that would read `wall_ms` from it is future work.
