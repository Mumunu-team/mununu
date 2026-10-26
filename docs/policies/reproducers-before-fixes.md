# Reproducers before fixes

> Status: policy (binding for defect fixes). `CLAUDE.md` carries the rule in short form; this file is the full text with the incidents behind it.

## The rule

> Source of truth: [`scripts/verify-repro.sh`](../../scripts/verify-repro.sh) (`make verify-repro`) — surface: CLI-only — a contributor instrument, run through `make`

A change that claims to fix a defect declares how the defect was reproduced, in a commit trailer, using exactly one of:

```
Repro: in-repo <test or fixture name>
Repro: consumer-only <who> — <why it cannot be reproduced here>
Repro: none — containment only
```

`Repro: none — containment only` may NOT accompany `closes #N` on a defect issue. Containment is legitimate and often urgent — a crash turned into a verdict, a budget into an abstention — but it does not establish a cause, so it leaves the cause issue open.

| | containment | cause fix |
|---|---|---|
| establishes a cause | no | yes |
| reproducer required | no | **yes** |
| may close the cause issue | **no** | yes |
| must say so | `Repro: none — containment only` | `Repro: in-repo …` |

Blurring the two lanes is the failure the rule exists to prevent: a containment that closes the issue retires the investigation while the defect is still there.

**What counts as a reproducer.** A test or fixture that **fails on the parent commit and passes on this one**. A regression test that passes on the parent is decoration — it locks in behaviour, it does not demonstrate a defect.

```bash
make verify-repro TEST=<test name substring> [REF=HEAD~1]
```

| outcome | exit | meaning |
|---|---|---|
| `CONFIRMED` | 0 | the test builds at `REF` and **fails** — a genuine reproducer |
| `FAIL — PASSES at REF` | 1 | **the case the rule most needs to catch**: decoration, not a reproducer |
| `INCONCLUSIVE` | 3 | the test needs API this commit introduced; **not a pass** — justify in the PR |

The script is advisory with a stated reach limit (measured 2026-09-13 on mununu#542 and #544, both `INCONCLUSIVE`): a Rust unit test lives inside the production file it tests, so the script copies the test-bearing file wholesale onto the parent, which copies the fix too. It reliably catches decoration whenever the test compiles against the parent and reliably refuses to bless anything else; it does not mechanically confirm most API-introducing fixes. The trailer and review are the enforcement; the script is an instrument. It needs a commit to diff — on an uncommitted tree it reports "no file changed between HEAD and HEAD".

**`consumer-only` is a real answer, not an escape hatch.** Some defects live in inputs we do not have (a consumer's proprietary RTL, a 1–2 % timing-dependent abort). Naming the consumer and the obstacle is honest; what it forbids is pretending. The PR says what evidence was obtained and from whom, and — where a derived artifact would make the defect ours permanently (a generated BTOR2, a minimised netlist) — records whether that was requested.

**Scope.** Defect fixes. A feature, a behaviour-preserving refactor or a doc change needs no trailer. When in doubt: does the change assert that something was *broken*?

A reproducer pins one site. The fix is expected to cover the site's siblings — every other place the same input shape or failure mode reaches — and to say so in the PR (`docs/policies/failure-paths.md`, Rule 3: seven PRs on the same arena-exhaustion panic, 2026-08 → 2026-10, each with a confirmed reproducer, each fixing one site).

## Why a field and not a paragraph

> Concept: the rationale; no code artifact.

The failure this prevents is not theorising — a mechanism is what makes an observation actionable. It is writing the mechanism at the **same confidence as the observation, in the same sentence, in a tracked file**. The observations survive because they are cheap to check and someone checked them; the mechanisms die because they are expensive to check and nobody did. Prose cannot hold that distinction under editing; a field can. (The articulation is monono's, after three retractions on mununu#543 — theirs, and ours.)

## The incidents

> Concept: the record that motivated the rule.

- `crates/mununu-core/src/clts/mod.rs` carried a custom `Drop for Clts` whose stated purpose was to avoid a stack overflow from `Vec<Vec<Transition>>` "recursively dropping deeply nested structures" — but dropping a `Vec` iterates its elements; 2,000 inner `Vec`s do not nest 2,000 frames deep. The impl defended against a mechanism that does not exist, with a confident comment and no reproducer.
- A consumer published a memory-pressure cause for an abort into four tracked files; their fifth run refuted it. Their diagnosis of their own error is the rule worth importing: **before using a measurement as evidence, ask what it would have shown if the hypothesis were false. If the answer is "the same thing", it is not evidence — and its agreement with you is precisely why it is dangerous.**
- Our own leading suspect for the same abort (`PredicateExpr`'s boxed recursion, whose depth is set by a Craig interpolant's conjunct count) fit the reported clue exactly and was killed by one afternoon's bisection: it needs ~32,000 nested conjuncts to overflow an 8 MB stack.
- mununu#677 (2026-10-14): a reproducer spelled `w == 1` passed on the parent because the defective pass resolved atoms by their bare name; the translator emits bare names, so the test had to as well. A reproducer that does not fail on the parent is a decoy, whichever side wrote it.
