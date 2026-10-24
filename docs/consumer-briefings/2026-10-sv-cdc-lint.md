# Consumer briefing — `sv cdc-lint`: a structural clock-domain-crossing lint, and a SymbiYosys recipe for the functional check (2026-10)

> Source of truth: [`cdc_lint`](../../crates/mununu-core/src/adapter/cdc_lint.rs#L129) — surface: (CLI+API) — `mununu sv cdc-lint` / `POST /api/v1/sv/cdc-lint`; [`scripts/cdc_formal_sby.sh`](../../scripts/cdc_formal_sby.sh)

**Audience:** rosf, monono, mununu-ui and anyone who lifts multi-clock RTL, or matches the text of the multi-clock refusal. P0.4 of the EmTech tutorial plan.

## TL;DR

- **New verb and route.** `mununu sv cdc-lint` (the shared SV lift flags, `--fail-on`, `--json`) and `POST /api/v1/sv/cdc-lint` (request `SvCdcLintRequest`, response `SvCdcLintResponse`, the same type the CLI prints; both schemas are new under `docs/api-schemas/`). Each row has a `kind` (`synchronized`, `combinational-before-sync`, `unsynchronized`, `multi-bit-bus`, `qualified-data`, `reconvergence`, `dual-clock-memory`) and an `outcome` (`holds`, `violated` or `unknown`). The worst row drives the exit code: 2 on a `violated` row by default.
- **The refusal message changes.** The multi-clock refusal from `adapter/yosys` now reads "Run `mununu sv cdc-lint` to classify the crossings, verify each domain on its own, or pass `--merge-clock-domains` …". A consumer that matches "Verify each domain on its own" must update the match. The refusal itself, its trigger and its exit code are unchanged.
- **New script.** `scripts/cdc_formal_sby.sh` runs a functional multi-clock check with SymbiYosys (`multiclock on`), directly or in `mununu-sva`. Its verdicts carry SymbiYosys's guarantees, not mununu's.
- **No verdict changes** on any existing verb.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | nothing on `sv verify-auto`; the refusal text above if a lane string-matches it | update a string match, if any; optionally add a `cdc-lint` lane for multi-clock designs |
| **monono** | same | same |
| **mununu-ui** | none | none (RTL verb, no UI affordance per the surface-parity rule) |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag and want the verb | the verb lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes, for the verb beside `sby` | the functional recipe runs here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not decide verdicts |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | no | additive surface, not used by rosf |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | no | additive surface, not used by monono |

## Verification steps for every consumer

```bash
mununu sv cdc-lint top.sv --top top --frontend slang | jq '.outcome, (.crossings[] | {kind, outcome, source, destination})'
scripts/cdc_formal_sby.sh --top top --mode bmc --depth 20 top.sv
```

## Provenance

- Branch `feat/cdc-lint`. Measured on hand-written netlists (unit tests) and on yosys output (`e2e_cdc_lint_decides_the_gray_step_of_a_bus`): a binary counter through parallel synchronisers is `violated` (the gray-step query finds the 1 → 2 step), the same counter gray-coded `holds`, and a single-bit flag through a 2-FF chain is `synchronized`. The script returned FAIL with a VCD on a one-cycle pulse through a bare 2-FF synchroniser, and PASS on a holding property, in `mununu-sva`.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- A native multi-clock lift (a per-domain enable rewrite with ratio constraints and metastability modelling) stays on the roadmap.
- Crossings that start at a primary input, asynchronous set / reset synchronisers and latches are not analysed; the report's `notes` say so on every run.
