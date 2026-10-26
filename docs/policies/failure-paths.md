# Failure paths: silence is a defect, sweep the siblings, read the diagnostics first

> Status: policy (binding). `CLAUDE.md` carries the three rules in short form under "Failure Paths"; this file is the full text with the incidents behind it. Written 2026-10-25 from the trend analysis of 119 mununu issues: the three largest recurring classes — a verdict degraded silently (16 issues), a crash where an abstention was due (12), a fix that left its sibling sites untouched (the chains #462 → #490 → #504 → #542 → #600 → #638 → #730 and #467 → #553 → #596 → #601 → #734) — share one cause: the failure path was treated as an edge case of the happy path instead of a surface with its own contract.

## Rule 1 — silence is a defect

> Source of truth: [`BottomReason`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) + [`VerificationNote`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: (CLI+API) — every `sv verify-auto` report

Every path on which the tool does less than it was asked must say so in the report, as a field or a note with the reason: an input it dropped (#459: a `--config-value` that matched nothing scoped every verdict to nothing, silently), a member that refused the design (#734: `Err → None` turned a refusal into an instant "inconclusive"), a budget that stopped a row (#467, #553, #596: a ⊥ with no budget named), a frontend it chose or fell back to (#466), a value it could not carry (#723: a trace cell wider than 64 bits dropped), a formula it could not parse (#764: evaluated as true). The reader of a report must be able to tell "decided" from "did not try" from "tried and could not" without the transcript.

What the rule asks of a change:

- a new `Err(_) => None`, `.ok()`, `.unwrap_or_default()` or `let _ =` on a fallible call in a verdict path is a review finding unless the discarded error is re-surfaced somewhere the consumer reads it;
- a new budget, cap, ceiling or skip gets a `BottomReason` / `SkipReason` arm or a note kind the same PR documents;
- a new fallback (a different frontend, engine, encoding) is recorded on the report (`diagnostics.*` or a `ScopeCaveat` note), never only logged.

`/failure-path-audit` lists the sites this rule covers.

## Rule 2 — a crash is a defect of the class, not the site

> Source of truth: `#![deny(clippy::unwrap_used, clippy::expect_used)]` on the engine modules (`crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs` and siblings, landing per file group under the plan of 2026-10-25) — surface: build

An engine that cannot finish abstains with a reason; it never panics, aborts or overflows the stack. The crash class was fixed eight times at single sites (#462 OOM in one `unwrap`, #490 SIGABRT under memory pressure, #504 stack overflow, #542 "the #462 sweep never reached this file", #543, #600, #638, #730) because each fix repaired the site that had just failed. The rule is mechanical where it can be: the lint denies `unwrap`/`expect` in engine code, and each remaining site becomes a typed error that reaches the existing abstention path (`AdapterErrorKind::ResourceBudgetExceeded`, a named refusal). Where a lint cannot see the pattern (recursion depth, an index computed from a width), the fix of one site lists the siblings — Rule 3.

## Rule 3 — sweep the siblings

A fix for a defect names every other site with the same pattern and either fixes it in the same PR or adds a test that enumerates them (`every_translator_atom_shape_has_a_known_seeding_classification` is the model: a table from "the producer can emit shape X" to "the consumer accepts shape X"). The PR body lists the sites it swept. Evidence: #705 → #708 (`disable iff` fixed for one body shape, the `##1`-leading body found a month later), #696 → #709 → #711 → #717 (a name bound to the wrong signal, fixed per construct four times), #578 → #609 → #741 (each engine kept its own reading of an initial value).

## Rule 4 — read the tool's own diagnostics before bisecting sources

When a lift or a run fails, the first instrument is the report: `diagnostics.frontend`, `frontend_fallback_reason`, `lift_retry`, `bottom_reason`, the notes. On 2026-10-22 a lift failure on OpenTitan `csrng` ("Found topological loop") was bisected for a day over the `INC_ASSERT` regions of twelve source files; it was yosys's BTOR backend tripping on yosys-slang's `$buf` cells, independent of every source change, and `check` reported no loop at any stage. A direct probe of the failing pass (replaying the lift script by hand on the kept staging directory, `MUNUNU_KEEP_YOSYS_TMP=1`) found it in one run. The order is: read what the tool reports → replay the failing stage by hand → bisect inputs last.

## See also

- [`reproducers-before-fixes.md`](reproducers-before-fixes.md) — the reproducer a sibling sweep's fixes still need.
- [`documentation-traceability.md`](documentation-traceability.md) — the anchors that keep this file's claims live.
