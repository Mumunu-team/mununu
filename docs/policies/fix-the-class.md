# Fix the class: a fix says which defect class it belongs to, and covers the class

> Status: policy (binding for defect fixes). `CLAUDE.md` carries the rule in short form under "Fix the Class"; this file is the full text with the incidents behind it. Written 2026-11-20.

## The rule

> Source of truth: [`scripts/pr_review_gate.py`](../../scripts/pr_review_gate.py) (`class_failures`; the `review-gate` commit status) — surface: CLI-only — a contributor instrument

A reproducer pins one input. The defect is usually wider than that input: the same code meets other cases of the same thing, and a fix scoped to the reproducer leaves them for the next issue. A pull request that fixes a defect therefore declares, in its description, exactly one of:

```
Class: <slug>
Class: none — <why this is a single site>
```

`<slug>` names an entry of [`docs/defect-classes.md`](../defect-classes.md). The entry may be added in the same pull request.

**Before writing the fix**, answer three questions, in this order:

1. **What varies?** Name the dimension the failing input is one value of — the kinds of free cell an atom's cone can reach, the reasons a portfolio member stops, the places a request can name a file the server reads. If you cannot name a dimension, the defect may be a single site: say why.
2. **What are the other values?** Enumerate them from the code that produces them (the enum, the parser's arms, the lift's cell kinds), not from memory. This is the catalogue entry's case table.
3. **What does this code do on each?** For each case: fixed here, already correct (name the test that shows it), refused with a reason, or open (file the issue and list it in the entry).

Then the fix covers the cases, and one test enumerates them — a table from "the producer can emit case X" to "the consumer handles case X", in the cheapest tier that can express it (`CLAUDE.md` §Test the Layer the Bug Lives In). When the cases come from an enum, the test matches on it exhaustively, so a new variant fails to compile until someone decides it.

**The catalogue entry is updated by every fix in the class.** A fix that meets a case the entry does not list adds the case; that is the signal the class was described too narrowly, and it is the cheapest moment to ask what else is missing.

**`Class: none` needs its reason** — a typo in one message, a constant with one use. "Small" is not a reason; the size of the diff says nothing about how many cases the code meets.

## What the gate checks, and what the reviewer checks

The gate checks form: a fix has exactly one `Class:` line, and a slug has an entry in the catalogue at the pull request's head. It cannot check that the entry's case table is complete. That is the reviewer's question in a cross-host review ([`cross-host-review.md`](cross-host-review.md)): read the producer of the cases and compare.

This rule sharpens Rule 3 of [`failure-paths.md`](failure-paths.md) ("sweep the siblings"). Rule 3 asks for the other **sites** with the same pattern; this asks first for the other **cases** the one site meets, and gives both a place to be written down.

## Why

Rule 3 has been binding since 2026-10-25 and the chains continued, because "the same pattern" was read as "the same line of code elsewhere". Measured on 2026-11-20, four chains of the previous two weeks were each a class fixed one case at a time:

- a free cell behind a property atom: the named cut net (mununu#1062), then the anonymous one (mununu#1086), then a name collision, in 21 hours;
- a file the server reads for a caller: the request's fields, then the paths inside a verify config (mununu#935), then `#include` directives inside the files (mununu#994), then the path separator;
- why a portfolio member gave no verdict: its own bound (mununu#1022), then the unused wall (mununu#1038), then a wall or budget stop reported as "a peer decided" (mununu#1043);
- an initial cube kept on doubt: one doubt measured (mununu#1061), then every doubt (mununu#1085), then the remedy per cause.

In each, the first fix's author had the producer of the cases open in the editor.

**Falsifier for the rule.** If the fix-of-fix count of the four weeks after the gate is required is not lower than the 11 of the two weeks before 2026-11-20, declaring the class did not change how the fix was scoped, and the rule is reworked or retired.

## See also

- [`../defect-classes.md`](../defect-classes.md) — the catalogue.
- [`reproducers-before-fixes.md`](reproducers-before-fixes.md) — the reproducer pins the one case; this rule covers the rest.
