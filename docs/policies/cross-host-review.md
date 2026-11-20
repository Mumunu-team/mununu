# Cross-host review: a sensitive change is reviewed on the other machine before it merges

> Status: policy (binding). `CLAUDE.md` carries the rule in short form under "Cross-Host Review"; this file is the full text with the incidents behind it. Written 2026-11-20.

## The rule

> Source of truth: [`scripts/pr_review_gate.py`](../../scripts/pr_review_gate.py) (the `review-gate` commit status, posted by `.github/workflows/review-gate.yml`) — surface: CLI-only — a contributor instrument

Work on this repository runs in sessions on more than one machine. A **sensitive** pull request merges only after a session on a machine other than the one that wrote it has reviewed its current head.

A pull request is sensitive when either holds:

- it adds or removes at least `min-changed-lines` lines under the prefixes of [`scripts/review-sensitive-paths.txt`](../../scripts/review-sensitive-paths.txt) — the code that decides or reports a verdict, what a server reads or runs for a caller, the licence check, and the gates themselves;
- it carries the `soundness` label.

Three lines carry the record:

| Line | Where | Written by |
|---|---|---|
| `Authored-on: <machine>` | the pull request description | the session that opens the pull request |
| `Cross-review: approve — host <machine>, head <sha>` | a pull request comment | the reviewing session, on the other machine |
| `Cross-review: changes-requested — host <machine>, head <sha>` | a pull request comment | the same, when the review has a merge-blocking finding |

`<machine>` is the machine's nickname (`MUNUNU_MACHINE_NICK`, the one the status board uses) — never a hostname, an address or a person's name. `<sha>` is at least the first seven characters of the head commit the review read.

**The review is of a commit, not of a pull request.** A push after the review moves the head, and the gate asks again. This is deliberate: the follow-up commit is where the second defect of a chain arrives.

**What the review is.** One `review-orchestrator` run scoped to the pull request, read-only, reading CI rather than starting a build. Its merge-blocking findings (a `[RULE]` violation, or a HIGH soundness / crash / silence site) make the verdict `changes-requested`; advisory findings go in the comment and do not block. The reviewing session reads the diff first and the author's description second, and checks the `Class:` declaration (see [`fix-the-class.md`](fix-the-class.md)): are the cases the class lists the ones this code can meet?

**Finding the work.** The gate labels a waiting pull request `needs-cross-review`. A session that starts, or finishes a task, runs `gh pr list --label needs-cross-review` and reviews those whose `Authored-on:` names another machine before it starts new work of its own.

**The waiver.** When the other machine cannot review (it is down, or the fix cannot wait), the founder may waive the review, in the session, for that pull request: `Cross-review: waived by the founder — <reason>, head <sha>`. A session never writes this line on its own judgement.

## What the gate can and cannot do

Both machines push from one GitHub account, so GitHub's "required approving review" cannot tell them apart — an account cannot approve its own pull request. The gate reads the three lines above instead and posts the commit status `review-gate`; branch protection requires that status. It refuses a sensitive pull request with no `Authored-on:` line, no review, a review from the authoring machine, a review of an earlier commit, or a review that asks for changes. `scripts/pr_review_gate.py --control` checks each of those refusals, and runs before every gate run.

It is a forcing function for sessions that follow the rules. It is not an access control: any session could write either line. A review line written on the authoring machine is a rule violation of the same weight as `--no-verify`.

## Why

Measured on 2026-11-20 over the fix commits of the two weeks before it: 27 fixes replaced code less than seven days old, 11 of them replacing an earlier fix. Two chains ran three fixes deep inside a day. In both, the review of the first fix was written on the other machine and arrived **after** the merge — its findings became the next commit on `main` (the cut-net sampling in the exact engine, mununu#1062 → #1086; the cycle-0 cube doubt, mununu#1085). The review was right both times; it was late both times.

The threshold was measured the same day on the 150 pull requests merged since 2026-11-06. At 100 changed lines the gate covers 71 of them and 10 of the 11 that began a fix-of-fix chain; at 250 it covers 35 and 5 of the 11. A path list alone covers 111, which is not a selection. The threshold lives in `scripts/review-sensitive-paths.txt` with those numbers beside it, and is re-measured when it is changed.

**Falsifier for the rule.** If, four weeks after the gate becomes a required check, the weekly count of fixes that replace code under seven days old has not fallen below half of the 2026-11-14 week's 17, the review is not what catches these defects and the rule is narrowed or retired.

## See also

- [`fix-the-class.md`](fix-the-class.md) — the class a fix declares, which the review checks.
- [`ci-and-hooks.md`](ci-and-hooks.md) — the gate that every pull request passes; this one is for the sensitive ones.
