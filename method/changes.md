# method/changes.md

Part of Kowloon, the method of `ALIGNMENT.md`. Read it before you change anything outside `alignment/`, or draft a proposal for later adoption.

## 8. Changing things

Alignment is read-only, so nothing should break. This section applies to anything you are later asked to change, and to the proposals you draft now. What you propose about `spec/` is drafted as a delta (section 12), which never words an item.

### What is a change of intent

A change of intent starts as a change to `spec/`, written by the maintainer through the intent cycle of section 3 and signed, before any derived work. The unit you name is the capability or the item that would change. A change that only makes derived work serve an existing item needs no spec change.

| The change | Is it a change of intent? |
| --- | --- |
| Adding a capability, a flag, an output field or a newly accepted input | Yes |
| Changing what an outcome code or a verdict means, or changing a default | Yes |
| Removing or weakening a promise, loosening a budget, adding or strengthening an assumption | Yes |
| Adding a dependency, a permission or a data flow a `constraint` does not allow | Yes |
| Promoting an experiment | Yes |
| Fixing code so that it does what an item already says | No |
| Refactoring, with every linked check unchanged and passing | No |
| Making something faster within its budget | No |
| Adding a check for an existing item, or correcting a document to match an item | No |
| Behaviour no item mentions, discovered while working | Not yours to decide. Record a candidate and ask |

**How the gate tells.** For a closed area the gate can list a contract surface: the signed files; the linked checks, their expected outcomes and their negative controls; and recorded listings of the interface (commands, flags, schemas, outcome codes, ports, parameters, tool permissions), of dependencies and of check restrictions. A change is treated as a change of intent, and fails without a new signature, when any of these is true:

- a file under `spec/`, or `.gitattributes`, differs;
- a linked check or a negative control is removed, renamed, or has a different expected outcome, or, for a property or an eval set, its content differs;
- a recorded listing differs from one regenerated from the change.

Adding a link or a check to an existing item at its current version is not a change of intent. What this cannot see is a change in behaviour that no check and no listing covers; characterisation checks and blind re-extraction are for that.

### Break nothing you are not changing

- **One unit per change.** Every change names one divergence entry or one spec item as its unit, and lists the paths and checks it expects to affect. This is a working rule for you; the gate checks only the baseline.
- **A task list for a change of more than one step.** Before starting, write `alignment/tasks/<unit>.md`: the unit, the paths and checks you expect to affect, and the steps in order, each ending in a check that can be run. Show it with your first report and tick steps as they finish. It breaks down work the maintainer already asked for; it gives no mandate of its own and is not intent. Delete it in the commit that completes the change.
- **Everything else stays as it was.** A check outside that list has the same status after the change as in the baseline.
- **Never silence a check.** Do not edit, delete, skip, ignore or loosen any test, waiver, threshold or CI setting outside the unit; nor inside it without the divergence identifier and the maintainer's decision.
- **A surprise failure is a finding.** If a check outside the unit fails, stop and report. Do not touch the test. The maintainer decides whether the scope was wrong or the change regressed something.
- **Existing tests are status quo.** A test that looks fitted to the implementation, or vacuous, is recorded as a candidate, not changed. The maintainer rules on these when the area closes.
- **No performance work without a violated budget** or a `purpose` that asks for it.
- **Proposals must not break the build.** Anything drafted for later adoption reports without failing in areas that are not closed.

### Assumptions

You may propose, in a delta, that an assumption is needed; the maintainer writes, justifies and signs it. With the proposal, give:

- the behaviour that depends on it, which will name it in `under:`;
- a generated line naming every check that went from failing to passing with the assumption, which shows when a failing promise is being turned into one;
- one witness, kept as a `check` line, in which every assumption of the behaviour holds together and the behaviour is exercised, so the assumptions are not so strong that nothing is checked;
- what real behaviour of the environment it excludes.

### Reading check

Before the maintainer signs something you proposed, ask one question about a consequence of the change, multiple choice or free form. For a property, show one trace and ask whether it is rejected. Do not hint. If the answer does not match, point to the relevant part and ask one different question; never a third. If the maintainer says your grading is mistaken, record `disputed` and move on. The check never blocks signing. Ask once per batch signed. Append one line to `alignment/read-checks.md`:

```
<id> | <what was signed> | attempts: 1|2 | outcome: matched | not-matched | disputed | skipped
```

## 9. Experiments

Assume the repository holds code that is built but not committed to.

- An experiment has a `hypothesis` item with a `review-by:` date and no checks.
- It is off by default. Linked checks and budgets run with experiments off and are unaffected by them. Existing experimental code with no switch is a finding; adding one is a change, made after its area is closed.
- Stable code never depends on experimental code. A `constraint` says so and the dependency listing checks it.
- On promotion the maintainer writes the real item. The experimental code is evidence for it, and is then regenerated, or reviewed like any other derived work.
- An experiment past its date is reported everywhere and fails the gate in a closed area. Extending the date is a version bump, so it is signed.
