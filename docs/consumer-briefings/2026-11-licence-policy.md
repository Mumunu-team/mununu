# Consumer briefing — what mununu does in each licence state (Track H1b, 2026-11)

> Source of truth: [`licence::decide`](../../crates/mununu-core/src/licence.rs) and the CLI's start-up gate [`enforce_licence`](../../crates/mununu-cli/src/main.rs) — surface: CLI+API (the server is the same binary).

> **Audience:** everyone who runs `mununu` — rosf and monono (CI gates in the `mununu-sva` image), developers with a persistent install, and self-hosted deployments.
>
> **Related:** the previous briefing, [`2026-11-licence-status.md`](2026-11-licence-status.md) (the status surfaces); [`docs/licensing.md`](../licensing.md); mununu v0.5.10 (status only, every state runs).
>
> **TL;DR — a behaviour change, decided by the founder on 2026-11-08: without a licence file, `mununu` runs for 30 days from the first run on a host and then exits 1 until a licence file is installed.** `mununu server` refuses to start in the same state. Every CI runner that starts from a clean image is on day 0 every run and is not affected; a persistent machine (a developer's laptop, a long-lived container with a mounted home, a build box) needs a licence file within 30 days. Licence files for the free tiers (academic, individual, open source) are issued like any other — write to the maintainers. **No verdict changes**: the gate runs once at start-up, before any engine, and the verdicts of a design are byte-identical in every state that runs.

## What changed

| state | v0.5.10 | now |
|---|---|---|
| `valid` | runs | runs |
| no file (`missing` in v0.5.10) | runs, reported `missing` | **`evaluation`** for 30 days from the first run on the host (`~/.mununu/first-run`): runs with one stderr line (`--quiet` hides it); then **`expired`**: `mununu` exits 1 with the renewal text, `mununu server` refuses to start |
| `grace` | runs | runs, with a loud stderr line on every start |
| `expired` (a file past its grace days) | runs | **exits 1** / the server refuses to start |
| `invalid` (bad signature, tampered, unknown key) | runs | treated as no file: the evaluation term governs; the line says why the file was refused |
| `missing` | any no-file case | only "no file **and** the first run could not be recorded" (an unwritable home): runs, says so |

The status surfaces keep their shape; `state` gains the value `evaluation`, and the status
document gains `evaluation: {started, ends, days_left}` where a term governs
(`licence-status.schema.json` and `sv-verify-auto-response.schema.json` regenerated — the
`licence` summary's `tier` reads `evaluation` and `expires` the term's end while on the term).
`mununu licence status` is never refused, so the reason is always readable.

## Per consumer

| consumer | what this means for you | action |
|---|---|---|
| **rosf** | Your gate runs mununu in a fresh `mununu-sva` container per job: the first-run stamp is written inside the container and dies with it, so every run is day 0 of the evaluation — nothing is refused, and one stderr line (`mununu: evaluation: 29 day(s) left …`) appears per invocation unless you pass `--quiet`. | None required. To silence the line, pass `--quiet` or mount a licence file (`MUNUNU_LICENSE_FILE`). A long-lived container or a mounted home directory is a persistent host and needs the file within 30 days. |
| **monono** | Same, through rosf and `make formal-docker`. | None required; same note. |
| **mununu-ui** | `/api/v1/health` may now report `state: "evaluation"` with `expires` = the term's end; when the server refuses to start there is no `/health` at all. | Handle the new value where the state is shown; nothing else. |
| **self-hosted deployments** | A persistent host needs a licence file within 30 days of its first run: `/etc/mununu/mununu.lic`, `[licensing] file` in `mununu.toml`, or `MUNUNU_LICENSE_FILE`. | Install the file; `mununu licence status` says `valid`. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` / the release images | Yes, to get the behaviour; the previous binary keeps running in every state. | A binary change (the start-up gate, the new state). |
| rosf `docker/Dockerfile*` | No. | rosf runs the mununu image; nothing in rosf changes. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | No. | Same. |

## Provenance

- Roadmap Track H, item H1's behaviour table (the recommendation the founder adopted on 2026-11-08) and the business plan's 30-day evaluation (§19, the 2026-10-23 proposal, adopted with this change). H1 (mununu#901) shipped the mechanism and the status; this is the policy.
- The gate is one function (`enforce_licence`) called once in `main`, exempting `mununu licence status`; `tests/licence_never_reaches_a_verdict_h1.rs` still holds (no verdict module reads the licence), and the policy table is a unit test (`the_decision_follows_the_table`).

## Not covered here

- Issuance (private); who receives which tier is the pricing page's matter, not this briefing's.
- The on-prem bundle (H5), which will carry `mununu.lic` beside `mununu.toml`.
