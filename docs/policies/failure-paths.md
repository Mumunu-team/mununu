# Failure paths: silence is a defect, sweep the siblings, read the diagnostics first, name a remedy the user can take

> Status: policy (binding). `CLAUDE.md` carries the rules in short form under "Failure Paths"; this file is the full text with the incidents behind it. Written 2026-10-25 from the trend analysis of 119 mununu issues: the three largest recurring classes — a verdict degraded silently (16 issues), a crash where an abstention was due (12), a fix that left its sibling sites untouched (the chains #462 → #490 → #504 → #542 → #600 → #638 → #730 and #467 → #553 → #596 → #601 → #734) — share one cause: the failure path was treated as an edge case of the happy path instead of a surface with its own contract.

## Rule 1 — silence is a defect

> Source of truth: [`BottomReason`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) + [`VerificationNote`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: (CLI+API) — every `sv verify-auto` report

Every path on which the tool does less than it was asked must say so in the report, as a field or a note with the reason: an input it dropped (#459: a `--config-value` that matched nothing scoped every verdict to nothing, silently), a member that refused the design (#734: `Err → None` turned a refusal into an instant "inconclusive"), a budget that stopped a row (#467, #553, #596: a ⊥ with no budget named), a frontend it chose or fell back to (#466), a value it could not carry (#723: a trace cell wider than 64 bits dropped), a formula it could not parse (#764: evaluated as true). The reader of a report must be able to tell "decided" from "did not try" from "tried and could not" without the transcript.

What the rule asks of a change:

- a new `Err(_) => None`, `.ok()`, `.unwrap_or_default()` or `let _ =` on a fallible call in a verdict path is a review finding unless the discarded error is re-surfaced somewhere the consumer reads it;
- a new budget, cap, ceiling or skip gets a `BottomReason` / `SkipReason` arm or a note kind the same PR documents;
- a new fallback (a different frontend, engine, encoding) is recorded on the report (`diagnostics.*` or a `ScopeCaveat` note), never only logged.

`/failure-path-audit` lists the sites this rule covers.

## Rule 2 — a crash is a defect of the class, not the site

> Source of truth: `#![deny(clippy::unwrap_used)]` on the engine modules (`crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs` and siblings; landed 2026-10-25 with the cube relation converted and the witness / strategy extractors marked `#[allow(clippy::unwrap_used)] // M1 pending`). An `expect` is permitted only with a message naming an invariant the code itself established ("filled just above", "validated above"); a value a design or a tool controls is never an invariant — `/failure-path-audit` lists every `expect` with its message) — surface: build

An engine that cannot finish abstains with a reason; it never panics, aborts or overflows the stack. The crash class was fixed eight times at single sites (#462 OOM in one `unwrap`, #490 SIGABRT under memory pressure, #504 stack overflow, #542 "the #462 sweep never reached this file", #543, #600, #638, #730) because each fix repaired the site that had just failed. The rule is mechanical where it can be: the lint denies `unwrap`/`expect` in engine code, and each remaining site becomes a typed error that reaches the existing abstention path (`AdapterErrorKind::ResourceBudgetExceeded`, a named refusal). Where a lint cannot see the pattern (recursion depth, an index computed from a width), the fix of one site lists the siblings — Rule 3.

## Rule 3 — sweep the siblings

A fix for a defect names every other site with the same pattern and either fixes it in the same PR or adds a test that enumerates them (`every_translator_atom_shape_has_a_known_seeding_classification` is the model: a table from "the producer can emit shape X" to "the consumer accepts shape X"). The PR body lists the sites it swept. Evidence: #705 → #708 (`disable iff` fixed for one body shape, the `##1`-leading body found a month later), #696 → #709 → #711 → #717 (a name bound to the wrong signal, fixed per construct four times), #578 → #609 → #741 (each engine kept its own reading of an initial value).

## Rule 4 — read the tool's own diagnostics before bisecting sources

When a lift or a run fails, the first instrument is the report: `diagnostics.frontend`, `lift_retry`, `bottom_reason`, the notes. On 2026-10-22 a lift failure on OpenTitan `csrng` ("Found topological loop") was bisected for a day over the `INC_ASSERT` regions of twelve source files; it was yosys's BTOR backend tripping on yosys-slang's `$buf` cells, independent of every source change, and `check` reported no loop at any stage. A direct probe of the failing pass (replaying the lift script by hand on the kept staging directory, `MUNUNU_KEEP_YOSYS_TMP=1`) found it in one run. The order is: read what the tool reports → replay the failing stage by hand → bisect inputs last.

## Rule 5 — a remedy is something the user can do, or it says there is none

> Source of truth: [`AdapterError`](../../crates/mununu-core/src/adapter/mod.rs#L568) + [`AdapterWarning`](../../crates/mununu-core/src/adapter/mod.rs#L502) + [`BottomReason`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: (CLI+API) — every refusal, abstention and warning text a report or an error carries

Added 2026-11-19 from an audit of about 90 refusal and remedy messages (mununu#1106). The reason a path was taken (Rule 1) is half of the contract; the other half is what the reader is told to do about it. The audit found messages that named a flag no verb has (`--variable-bounds` on the XState adapter; `--timeout-ms` in an `sv verify-auto` note), a flag of a different verb (nine exact-engine refusals offering `--engine explicit`, which `btor2 verify` rejects), an input the verb does not take ("supply an `init` via a sidecar" on a verb with no sidecar), a mechanism that was retired (the antecedent shadow register, after inputs-in-the-state), two versions of one requirement twenty lines apart (yosys ≥ 0.40 and ≥ 0.67), and about forty that offered a Rust identifier, a roadmap step or a tier name as the fix. A wrong remedy costs the user more than no remedy: they spend the time, and then stop trusting the next message.

What the rule asks of a message that reaches a user (an `AdapterError`, an `AdapterWarning`, a `BottomReason` text, a `VerificationNote`, an API error body, a CLI diagnostic):

- **Every remedy it names can be taken on the surface that produced it.** A flag exists on the verb the user ran; an environment variable is in [`docs/env-vars.md`](../env-vars.md); a sidecar key or a DSL construct is one the parser accepts. A message built in code several verbs share takes the verb as an argument, or names no flag.
- **When there is nothing to do, it says so, and says whose limit it is**, in one of three forms: *a limit of the technique* (a BDD node budget, the GR(1) fragment — name the member or the verb that reads the shape, if one does); *a mununu limitation, nothing to change in the design* (an operator, an SVA form, a memory shape the engine does not carry yet); *a mununu defect, please report it* (malformed output of our own lift, a violated internal invariant, a formula we emitted and cannot parse). "Reshape the property" without the accepted shapes is none of the three.
- **It speaks in terms the user typed.** Signal, automaton, label and property names from the input; never a Rust type or variant name, a `Debug` rendering (`{:?}`) of an internal value, a roadmap step, a phase or a tier.
- **An "unknown X" lists the candidates**, or the nearest ones when the set is large.
- **It is checked like any other claim.** The PR that adds or changes a remedy follows it once (the flag parses on that verb; the key is accepted) and says so; a remedy that names a retired mechanism is removed in the commit that retires the mechanism, as an anchor is (see [`documentation-traceability.md`](documentation-traceability.md)).

Classifying a gap as a limit of the technique or as ours is part of writing the message, and the answer is recorded where the capability is planned, not only in the text: a gap that is ours and small is a roadmap row, and a message that says "not supported yet" for it is honest only while that row exists.

`/failure-path-audit` class C lists the sites this rule covers.

## See also

- [`reproducers-before-fixes.md`](reproducers-before-fixes.md) — the reproducer a sibling sweep's fixes still need.
- [`documentation-traceability.md`](documentation-traceability.md) — the anchors that keep this file's claims live.
