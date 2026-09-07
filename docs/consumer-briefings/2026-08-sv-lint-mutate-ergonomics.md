# Consumer briefing — 2026-08 `sv lint` / `sv mutate` ergonomics (mununu#475)

> **Audience:** primarily **monono** (the adoption reporter for `sv lint` / `sv mutate`), secondarily **ROSF** (touches CLI/API surface parity but no verdict semantics).
>
> **Fix:** mununu#475. **User-facing docs:** [`docs/verifying-rtl.md`](../verifying-rtl.md) §§`sv lint`, `sv mutate` — the "Recent additions" callouts.
>
> **TL;DR:** four ergonomics gaps in `sv lint` / `sv mutate` closed; one docstring clarified. Item 3 (single-file cross-directory module resolution) deferred to a follow-up. No verdict semantics change; parser surface adds two new CLI flags. Consumer action is opt-in — new flags let previously-blocked workflows land.

## What changed

- **Item 1 — `sv lint` false-positive on `function automatic` args.** `ctrl_code.c`, `ones8.v` and similar function-arg-mangled names in yosys/slang output no longer show up as partial-write register findings. Filter is the `.` heuristic on Op-node symbols (register aliases + hierarchical State names never carry dots; function args always do). One new regression test in `sv_verify.rs`.
- **Item 2 — `--exclude <NAME>` on `--design-dir`.** Case-insensitive path-component match; skips a named directory anywhere in the scan tree on top of the hardcoded `mutations`/`buggy`/`buggy_artifacts`/`tb`/`testbench`/`sim`/`figures` list. `--exclude faulty` was the reporter's use case. Repeatable. Not a full glob (a richer syntax is a future extension). Two new tests in `source_manifest.rs`.
- **Item 4 — `--param NAME=VALUE` on `sv mutate`.** Parity with `sv verify-auto`'s `--param`. Same parser, same yosys `chparam` / slang `-G` plumbing. Blocks whose adequacy measurement had to shrink a parameterised timing interval can now be mutated end-to-end.
- **Item 5 — `sv lint --help` exit-code doc.** New docstring on the `Lint` command variant explains: exit `0` = clean, exit `2` = at least one finding, exit `1` = lift failure. Batch scanners that want to distinguish "found something" from "could not lift" should pass `--fail-on none` and inspect the JSON/text output.

**Item 3 (single-file `sv lint` cannot resolve cross-directory submodules)** — deferred to a follow-up PR. It needs deeper design thought around yosys/slang module search-path semantics (`--search-path <DIR>` vs `--filelist <PATH>`). In the meantime, the item 2 `--exclude` closes the reporter's whole-tree workflow via `--design-dir <parent> --exclude faulty`, which was the operative unblock.

## What did NOT change

- **`PropertyVerdict`** — unchanged.
- **`AutoVerifyReport`** — unchanged.
- **Existing CLI flags** — unchanged shape and semantics.
- **HTTP API routes** — unchanged. `--exclude` and `--param` on `sv mutate` are CLI-only additions in this PR (no API/UI parity yet — additive follow-up).
- **Engine behaviour on any previously-decided property** — unchanged.

## For monono-agents (direct CLI consumer)

**What to update on your side:** at minimum, nothing — old commands still work. To adopt the new flags:

- **Whole-tree lint over a `faulty/` sibling tree**: `mununu sv lint --design-dir <root> --exclude faulty`. Previously blocked because slang rejects the deliberately-invalid twins; now unblocked without touching the hardcoded skip list.
- **`function automatic` false-positive purge**: no action needed — the fix is automatic. Any allowlist entry your CI carried for `<fn>.<arg>` findings can be dropped (and *should* be dropped, because an allowlist entry for a false positive is the thing most likely to later hide a true one — reporter's exact phrasing).
- **Mutate parameterised blocks**: `mununu sv mutate <file> --mutation stick:<reg> --param ROW_BITS=2` and similar. The blocks that most need adequacy measurement — the ones sized by parameters — are now reachable.
- **Exit-code discipline**: your batch scanner should either accept the new gate default (exit 2 = finding, exit 1 = lift failure) or pass `--fail-on none` and read JSON. Documented in `sv lint --help`.

**What to expect:** the reporter's `tmds_encoder.sv` case should now report zero findings on `ctrl_code.c` / `ones8.v`. The whole-tree `--design-dir <root> --exclude faulty` scan should complete without slang rejecting the twins. Any mutation on a parameterised block that previously couldn't lift under the mutate path should now lift with `--param`.

**Docker rebuild:** monono's Docker (if any) needs rebuild only if it pins a specific mununu binary version. Otherwise the ergonomics fixes pick up on the next binary pull. No sidecar or config change required.

## For ROSF-agents (subprocess `--profile industrial`)

**What to update on your side:** nothing required. This PR doesn't touch the `--profile industrial` verify-auto path.

**What to expect:** if your industrial profile ever invoked `sv lint` or `sv mutate` (e.g. as a preflight before verify), the new flags are available. Otherwise, no observable change.

**Docker rebuild:** `rosf/docker/Dockerfile` needs rebuild only if it pins a specific mununu binary version. Otherwise no action.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod CLI) | binary carries two new CLI flags + a lint-filter fix | Yes if consumers pin a version tag |
| mununu `Dockerfile.dev` | binary bump | Only if the dev workflow requires the new binary |
| mununu `Dockerfile.sva` | binary bump; no e2e-test behaviour change (the shipped `#[ignore]`d e2e tests do not exercise `sv lint` / `sv mutate` cross-directory paths) | Only if a downstream requires the new binary |
| mununu `Dockerfile.extract`, `.extract-*` | no impact | No |
| rosf `docker/Dockerfile` | consumes mununu subprocess; no verify-auto or verdict-semantics change in this PR | Only if pinned to a version tag; behavior updates on next binary pull otherwise |
| rosf `docker/Dockerfile.dev`, `.hw` | no direct dependence on the changed paths | No |
| monono Docker (any) | consumes `sv lint` / `sv mutate` CLI | Only if pinned to a version tag |

## Shared footer — verification you should run after adopting

1. **Confirm the false-positive purge**: `mununu sv lint tmds_encoder.sv --frontend slang` on any module with `function automatic` should report zero function-arg findings.
2. **Confirm the whole-tree unblock**: `mununu sv lint --design-dir <root> --exclude faulty` runs to completion where `--design-dir <root>` alone previously errored on the twins.
3. **Confirm `sv mutate --param` parity**: `mununu sv mutate <file> --mutation stick:<reg> --param KEY=VAL` accepts the parameter and the mutant re-verifies. A malformed `--param` should be a hard error, not a silent drop.
4. **Read the `--help`**: `mununu sv lint --help` documents the exit-code semantics explicitly; batch scanners that were using the old default without knowing exit-2 was "finding" should sanity-check they handle it as intended.

## Provenance

- Fix commit: (pending merge — see branch `fix/475-sv-lint-mutate-ergonomics` in the mununu repo)
- Issue: mununu#475 — <https://github.com/Mumunu-team/mununu/issues/475>
- Policy this briefing was written to satisfy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md)
- Sibling briefing (mununu#476 antecedent shadow-synth): [`2026-08-antecedent-shadow-synth.md`](2026-08-antecedent-shadow-synth.md)

## Not covered here (follow-ups)

- **Item 3** — single-file `sv lint` cannot resolve cross-directory submodules (53/109 files in monono's tree lift standalone). Needs `--search-path <DIR>` (or `--filelist <PATH>`) design work; separate PR.
- API/UI parity for `--exclude` and `--param` (currently CLI-only additive).
- Full glob syntax on `--exclude` (currently path-component equality; suffices for `faulty/`).
