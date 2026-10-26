# Cross-Repo Impact Reporting Policy

> **Rule.** Every PR that changes user-visible behaviour of a mununu surface consumed by a downstream repo (ROSF, monono, mununu-ui, or any future consumer) must ship a **consumer briefing** — a single markdown file under [`docs/consumer-briefings/`](../consumer-briefings/) — that names the consumers affected, explains what changed for them, and states which Docker images (if any) need rebuilding.
>
> **Why.** mununu is one node in a small network of tools that build on top of it. Verdict semantics, JSON shapes, CLI flags, exit codes, sidecar formats, and engine behaviour all propagate to at least one consumer today (ROSF `--profile industrial` subprocess consumer; monono direct CLI consumer). Without a machine-checkable disclosure discipline, a "correct-in-isolation" mununu change silently breaks a downstream — or, more often, silently improves it in ways the downstream never notices and never validates. Both failure modes have happened.

## When this policy fires

A PR fires this policy when it does any of the following:

1. Changes the value of any `PropertyVerdict` variant, or the message strings that accompany a `Skipped` / `Unknown` verdict.
2. Changes the shape of any struct that `render_verify_auto_json`, `synthesis.diagnostics.to_json_value`, or any `POST /api/v1/*` handler serialises.
3. Adds, removes, renames, or reorders any CLI flag on `context`, `sv`, `btor2`, `verify`, `contract`, or `codesign` (and their sub-verbs).
4. Adds, removes, or reorders any HTTP API route under `/api/v1/`.
5. Changes the semantics of a shipped verdict — a property that used to `Skipped` now decides, or a property that used to `Holds` now `Violated`s or vice versa. Bugfixes count; a soundness fix is the most important case to report, not an exception.
6. Changes what tools mununu invokes as subprocesses (`slang`, `sv2v`, `yosys`, `pono`, `btormc`, `cvc5`, `verilator`) or the versions it pins in `docker/Dockerfile*`.
7. Changes any sidecar schema (`.mununu.json`, `@mununu_predicate` / `@mununu_config` / `@mununu_abstraction` annotations, `verify.toml`).

**Exemptions:** internal refactors that preserve every observable behaviour listed above; test-only changes; changes to `docs/` that only clarify existing behaviour; changes to `REVIEW_LOG.md`, `ONBOARDING.md`, or `scratchpad/`.

## What a consumer briefing must contain

At minimum:

1. **Audience banner** naming every downstream consumer affected (today: ROSF, monono; add others as they appear).
2. **TL;DR** paragraph — the behaviour change in one sentence, plus a phrase telling every consumer whether their action is required or optional.
3. **What changed at engine level** — one paragraph. Cite the fix commit, the issue number if one exists, and the design doc if the change is non-trivial.
4. **Per-consumer section** — one per audience — covering:
   - *What to update on your side* (may be "nothing required" — say so explicitly).
   - *What to expect* (verdicts that change, errors that go away, new fields that appear).
   - *Report parsing impact* — additive-safe changes get flagged as "no action needed"; breaking shape changes require an explicit migration cheat sheet.
   - *Docker rebuild disposition* for that consumer's images (see mandatory table below).
   - *Test-the-transition steps* — a concrete reproducer the consumer can run to confirm the change on their side.
5. **Docker rebuild table** — mandatory, verbatim shape. Columns: `Image | Impact | Rebuild required?`. Rows cover every Dockerfile in `mununu/docker/`, every Dockerfile in each consumer's `docker/` directory, and any consumer-side ad-hoc Docker files. Missing rows are a lint failure; blank rows are a lint failure.
6. **Shared footer** with verification steps every consumer should run.
7. **Provenance** — fix commit sha (or "pending merge — see branch `<name>`"), issue link, design-doc link, policy link.
8. **Not covered here** — an explicit list of follow-ups this briefing does NOT address, so consumers know what's still open.

## Where the briefing lives

`docs/consumer-briefings/YYYY-MM-<slug>.md`. Slug is a short kebab-case description of the change (`antecedent-shadow-synth`, `verify-auto-input-refusal`, `sv-lint-exclude-glob`). One file per PR that fires this policy; a batched PR that changes multiple surfaces gets ONE briefing that covers all of them, so consumers read exactly one document per adoption round.

## Docker rebuild disposition — how to fill the table

For every image in scope, state the impact and the rebuild disposition. Standard verdicts:

- **Yes if consumers pin a version tag** — the mununu binary changed; consumers that pin a specific tag must rebuild to pick it up.
- **Yes — mandatory** — the change requires the image be rebuilt AND its e2e tests re-run before merge (typically applies to `mununu-sva` for slang-gated tests per CLAUDE.md §"SVA-verification e2e validation").
- **Only if the dev workflow requires the new binary** — for `Dockerfile.dev` and consumer dev images.
- **No** — image is unaffected.
- **Behaviour updates on next binary pull** — for consumers that fetch the latest mununu binary on rebuild rather than pinning.

Never write "Maybe" or leave blank. If the impact is genuinely unclear, that's a signal the design isn't ready to ship.

## One ask, one issue — the consumer-ask protocol (2026-10-25)

A consumer (monono, rosf, mununu-ui) that finds a defect or needs a capability in an upstream repository files it **once, in the upstream repository**, with the consumer's evidence (the design, the command, the two verdicts). The consumer's own tracker carries a one-line pointer to the upstream number, never a copy. A later re-filing of the same ask — by either side — is closed as a duplicate pointing at the original, so the upstream fix closes exactly one issue and the trend record counts each defect once.

Evidence: on 2026-09-30 monono's consumer asks were re-filed upstream as mununu #596–#601 and rosf #55–#58, each a second copy of an issue already open or already fixed (#596 = #467, #597 = #496, #598 = #497, #600 = #462, #601 = #548; rosf #55 = #13, #56 = #14, #57 = #27). Half the day's filings were duplicates, and the trend tables of 2026-10-25 had to be de-duplicated by hand.

The origin label (`found:monono`, `found:rosf`, `found:opentitan`, `found:hwmcc`, `found:internal`) records where the defect was found; the class label records what it is. Both are set at filing.

## Enforcement

Today (2026-08): manual — reviewers check that any PR firing the criteria above ships a briefing. Failing to include a briefing on a qualifying PR is a review comment, not (yet) a hard CI gate.

Planned:

- `make docs-audit` extends its cadence check to warn when a load-bearing surface changes without a matching briefing in the same PR.
- Skill: `/cross-repo-impact-check` walks the diff, decides whether the policy fires, and either produces a briefing skeleton or asserts a matching briefing is present.
- CLAUDE.md rule: agents drafting a qualifying PR must produce the briefing as part of the PR, not as a follow-up.

## Historical context

This policy formalises a rule that had been implicit but repeatedly missed:

- **mununu#476** (2026-08) — soundness fix that changed verdicts on a class of SVA `|=>` properties. Consumers (monono especially) had been coding against the old — wrong — `Violated (1 cell)` results. The Phase A refusal + Option C shadow-synth PRs shipped with a briefing under this policy at [`docs/consumer-briefings/2026-08-antecedent-shadow-synth.md`](../consumer-briefings/2026-08-antecedent-shadow-synth.md) — the first briefing under the policy, and the case that motivated writing it down.

Prior surface changes shipped without briefings, and consumers discovered the impact by re-running their tests. That model does not scale as more consumers appear.

## What this policy is NOT

- **Not a versioning policy.** Semver / release tagging is a separate discipline (currently: mununu is 0.4.x, all releases treated as breaking).
- **Not a schema-freeze policy.** Consumers should still expect additive schema changes on `SvVerifyAutoResponse` etc.; the briefing tells them when a change is additive (safe to ignore) vs breaking (must migrate). The pinned wire format lives at [`docs/api-schemas/`](../api-schemas/) — machine-readable `*.schema.json` files (JSON Schema Draft-07, derived from the Rust types via `schemars`) alongside the human-facing `verdict.md`. The drift-detector at `crates/mununu-core/src/api/schema.rs::tests::api_schema_drift_*` fails CI on any wire-format change without a schema update, so `docs/api-schemas/` is authoritative. Briefings document *transitions* against those files.
- **Not a "communicate every change" policy.** Internal refactors, test additions, and documentation-only changes do not fire it. The policy targets the small set of changes that actually cross the mununu boundary.
- **Not a substitute for the mununu wiki or `docs/verifying-rtl.md`.** The wiki documents behaviour steadily; briefings document *transitions*.

## See also

- [`CLAUDE.md`](../../CLAUDE.md) — the binding rules; this policy is referenced from there.
- [`docs/policies/claims-integrity.md`](claims-integrity.md) — sibling policy on how mununu talks about its own capabilities. Both policies exist because getting the second-order effects wrong (what downstream consumers assume from a change, what the outside world assumes from a claim) has been more expensive than any first-order engineering mistake.
- [`docs/verifying-rtl.md`](../verifying-rtl.md) — the standing user-facing documentation. When a briefing lands, `verifying-rtl.md` should also be updated so the durable docs reflect the new behaviour.
