# method/intent.md

Part of Kowloon, the method of `ALIGNMENT.md`. Read it before you propose a class or a link, extract or draft a candidate, or take part in the intent cycle.

## 3. Intent

The aim is that every artifact can be traced to an intent a human owns, a named person can explain it, and anything that fails those two tests is eventually removed, by the maintainer's decision. Until alignment reaches a path, that path has no confirmed intent.

### Classes

Only text in the normative class is confirmed intent.

| Class | Contents | How you treat it |
| --- | --- | --- |
| `normative` | Files under `spec/`, once signed by the maintainer | The only source of confirmed requirements |
| `imported` | External references: standards, datasheets, interface definitions, vendor IP, pretrained weights, hosted models | Authoritative only through a signed spec item that interprets them |
| `derived` | Source, tests, proof scripts, and any document that describes what the product does | Must link to a spec item. Until it does, it is unassigned |
| `status-quo` | Files that instruct agents working on this repository, and existing memory, context and log files. Agent files the product ships are derived | Followed as working practice under section 2. Each rule in them is potential intent |
| `informative` | Comments, commit messages, plans, issues, agent notes, prose that only explains or records, the files under `alignment/` | Explanation and evidence only. Needs no intent |

- A comment that says "must", "always" or "never" is evidence, not a confirmed requirement.
- A plan item is not a mandate (section 7). Implement nothing from a plan.
- The method's own files carry no intent of the product, so their path class is `informative`. Their rules bind all the same, from the day they are copied in.
- A status-quo rule tells you how to work today. It does not tell you what the project's intent is.
- A source that lives outside the repository cannot carry a path attribute. The inventory lists it with where it lives, and that listing gives it its class.

### Units of intent

Intent is written at three levels: the project; each interface capability, as one item covering every interface that exposes it; and each package or module. Items for a file, a type or a function are written only to anchor a check that the module item cannot. A detail item with no `check` line is reported.

**Strategies are units too.** Three units cut across the capabilities: assurance (how the project tests and verifies), compliance (what it is bound by and the evidence it keeps) and documentation. `BINDINGS.md` names them, and may split one or add others, such as evaluation or security. Each one that exists goes through the intent cycle at project level, in the first pass (section 7).

- Your line and your paragraph describe the strategy the artifacts show, not the one a document claims: what kinds of check exist and what they leave out, what must pass before a merge or a release, which standards are cited and what evidence is kept, who the documents are for and what keeps them true.
- Where you find no strategy, say "none found". Do not invent one.
- **The first pass is for understanding.** There the maintainer writes only a `purpose` and, where something is left out on purpose, a `refusal`.
- **Binding rules come when an area closes, and bind that area.** They may be of any kind. The closing statement says, for each strategy, "rules written" or "none" (section 7).
- **A strategy item is not a working rule.** It states evidence the product must have, whoever does the work: "no test touches the network". A working rule states how an agent behaves: "run the linter before committing". Working rules stay in the instruction files.
- A strategy is not an area.

### The intent cycle

This is the one way intent is found and written, whatever the occasion: extraction of existing code (section 7), a change of intent or a new capability (section 8), promoting an experiment (section 9).

1. **Name the unit.** Say which unit is under alignment: its level and what it is.
2. **One line.** Ask the maintainer to write, on paper, one line of intent for that unit. Wait until the maintainer says it is written. Then show the one line you think the unit's intent is.
3. **One paragraph.** Ask the maintainer to write, on paper, a paragraph of intent for the same unit. Wait until the maintainer says it is written. Then show your paragraph. At detail level this round is skipped, and the tutor answers what the unit is for.
4. **The maintainer writes the spec.** After comparing, the maintainer writes the unit's spec items under `spec/`, in their own words, and signs them.

- The paper stays with the maintainer. Do not ask what it says, do not ask for a copy, and do not look for it.
- Never show your line or your paragraph before the maintainer has said theirs is written.
- Your line and your paragraph are inference, kept as evidence. You never word a spec item, and nothing of yours is ratified into `spec/` as written.

### Spec items

Written and signed by the maintainer, at the end of the intent cycle. Never by an agent.

```
id: SPEC-<area>-<nnn>
version: <n>
level: project | capability | module | detail
parent: <the spec item one level up, or none at project level>
statement: <one sentence: what must be true>
kind: purpose | refusal | behaviour | constraint | budget | assumption | hypothesis
checked-by: <check | listing | measurement | witness | review>, …
under: <assumption ids this behaviour depends on>      (only when there are any)
review-by: <YYYY-MM-DD>                                 (hypothesis only, and required there)
explained-by: <person>                                  (only when it is not the tutor)
```

The kind says what sort of statement the item makes. How it is verified is `checked-by`.

| Kind | Says | Usually checked by |
| --- | --- | --- |
| `purpose` | What the project, or a capability, is for: who needs it and what they lose without it. Orders work and informs rulings; never gates | Review |
| `refusal` | A non-goal: what the project, a capability or a strategy will not be or do. Anything that can be checked is a behaviour or a constraint instead | Review |
| `behaviour` | What a capability does or promises, including what it never does | A check |
| `constraint` | A rule that can be decided from the artifacts without running the system: allowed dependencies, tool permissions, licences, clock-domain and reset rules, data that may leave the host | A listing or a linter |
| `budget` | A numeric threshold: run time, memory, size, a pass rate. A property that is not a number, such as determinism, is a behaviour. | A measurement |
| `assumption` | What the environment must provide for a behaviour to hold. | A witness run |
| `hypothesis` | A question an experiment is meant to answer. | Nothing, until promoted |

- **Universal claims.** A behaviour whose statement says "always", "never" or "for all" is not covered by example tests alone. It needs a property, a differential run or an exhaustive check. A claim that something eventually happens carries a bound when tested; when model-checked, its fairness assumption is in `under:`. The maintainer judges both when signing; the gate cannot.
- **The main promise is written down.** A project whose value rests on a correctness claim states that claim as a project-level behaviour. `BINDINGS.md` names it.
- **Assumptions are tied to what depends on them.** Every assumption is named in the `under:` of at least one behaviour. One that is not is reported.

**Retirement.** When a capability is removed, its items are deleted, not kept as history. An id is never reused. A `refusal` may remain to say the capability is gone on purpose.

**When the version is bumped.** Bump when a reader who had verified the old text would have to verify again: the statement asks for something different, more or less; a term in it now means something else; `level`, `parent`, `kind` or `checked-by` changes. Do not bump for spelling, formatting, a clearer wording of the same requirement, or a change of `explained-by`. When in doubt, bump.

## 4. Binding and findings

Every derived path gets the same binding: its module item and link, detail items where a check needs them, checks with negative controls, and the tutor to answer for it. There is no lighter tier. A path whose intent is a `hypothesis` item is an experiment, and section 9 governs it instead.

Where code dominates run time, a simple reference implementation is worth keeping if one exists or is cheap to write. Agreement of the optimised code with it is then a check.

**Findings.** Two kinds of thing are generated by triage and by the release report, never assigned. Each is recorded in the finding format of section 12 and has three exits: the maintainer writes an item for it, it is an experiment, or it is removed.

| Finding | What it is | Until it exits |
| --- | --- | --- |
| unexplained | Reachable code no one can explain, behaviour no item asks for, or interfaces or documents that disagree | Frozen: no change without the maintainer, and no new code may depend on it (checked by review). A dated question to the maintainer, paths that can change a result first; reported at every gate run while overdue |
| unreachable | Reached by no capability, duplicated or redundant | Listed as "possibly unreachable" until the maintainer has ruled out an experiment. Then removed in a batch the maintainer signs (section 7) |

## 5. Links and checks

Each fact is recorded once.

**`.gitattributes` is the record for paths.** It gives every tracked path a `class`, and every derived path an `intent`, by pattern where a directory is uniform. `intent` is `unassigned`, or one or more spec ids with the version the link was made against, separated by commas with no spaces:

```
*                  class=derived       intent=unassigned
spec/**            class=normative
alignment/**       class=informative
AGENTS.md          class=status-quo
src/parser/**      class=derived       intent=SPEC-PAR-001@1
src/report/**      class=derived       intent=SPEC-REP-001@2,SPEC-REP-004@1
notes/**           class=informative
```

A later line overrides an earlier one, attribute by attribute, so every path the first line catches by mistake needs a line of its own: `.gitattributes` itself and the method's files are `informative`. Only `.gitattributes` gives a path intent.

**The trace map is the record for what is finer than a path.** One line per link, kept beside the spec, so that no source file is edited to record a link:

```
<spec id>@<version>   symbol   <path>::<function, type, or section heading>
<spec id>@<version>   check    <runner>:<the check's name as that runner lists it>[=<expected outcome>]
```

For example, `SPEC-FIFO-003@1   check   formal:fifo_top.a_no_overflow=proved`. An outcome other than the expected one, such as a timeout or a pass bounded to some depth, is not a pass.

A `symbol` line may only narrow. Its item must be the one its path already links to, or a descendant of that item through `parent:`. A map line never gives intent to a path that has none, which is why the map needs no signature.

**A link is stale** when the spec item's version is higher than the version in the link.

**One mechanism for everything that verifies.** A link to a check needs one thing: a stable name that a tool can list.

| Kind of check | Its name | The tool that lists and runs it |
| --- | --- | --- |
| A test | The test's full name | The project's test runner |
| A named assertion in a hardware design | The assertion's label | The verifier, which reports a verdict per label |
| A property of a design model | The formula's name in the model | The design checker |
| A budget | The measurement's name | The script that measures and compares |
| A runtime monitor or enforcement point | The monitor's name | The harness that injects a violation and observes the monitor act |

An assertion written inline in ordinary code has no stable name. It is covered by the `symbol` line of its function and by that function's tests. If it matters enough to link, it is given a test, and the test is the link.

- **Every check can fail.** A check that carries intent keeps a negative control beside it: a planted violation, a failing parent commit, a mutation, or a broken variant of a model. The control runs under the check's own assumptions, bounds and harness. A check with none is reported as unproven, not as passing.
- **Which tests need a line.** A test that protects a signed statement has a `check` line. A test with no line belongs to its path's `intent` and may change with the code it tests.
- **Characterisation checks.** A `check` line whose id is a candidate pins current behaviour and not intent. Write these before refactoring thinly covered code. When the candidate is ruled on, the line moves to the spec item or the check is removed.
- **Budgets.** Two or three per capability, measured with experiments off. Prefer deterministic counters to wall-clock time, which is recorded as a trend unless time is the promise (a deadline, timing closure). Compare against the alignment baseline, not the previous commit; the baseline is a starting point, not approval of the size. Loosening a number is a version bump.
- **Design properties (optional).** Where a design-level statement can be modelled, the model's properties are checks like any other, with a broken variant of the model as the negative control. This checks the model; that the code behaves like the model rests on the tutor's answer, or on an extractor where one exists.
