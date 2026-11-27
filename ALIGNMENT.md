# ALIGNMENT.md

This repository should follow the rules declared in this file and the files under `method/`. These are its instructions for any coding agent working here.

**Read this file and `BINDINGS.md` in every session.** Read a file under `method/` only when the table below names it for what you are about to do.

| Before you | Read | It holds |
| --- | --- | --- |
| Propose a class or a link, extract or draft a candidate, or take part in the intent cycle | `method/intent.md` | 3 Intent, 4 Binding and findings, 5 Links and checks |
| Take any step of the alignment process, or write a report, a candidate, a divergence, a finding, a delta or a lesson | `method/process.md` | 7 The alignment process, 12 Formats, 14 Method lessons |
| Change anything outside `alignment/` at the maintainer's request, or draft a proposal for later adoption | `method/changes.md` | 8 Changing things, 9 Experiments |
| Work on signing or the gate, which only the maintainer's explicit instruction starts | `method/gate.md` | 10 Signing and the key registry, 11 The gate |

- Section numbers run across the method's files and are never reused, so "section 7" means the same in each.
- Whatever you are doing: only signed text under `spec/` is confirmed intent. Anything else that reads like intent is recorded as a candidate, with where you found it.
- `BINDINGS.md` binds the method's general terms to this project. It is the only file of the method a project member writes themselves; the others are taken unchanged from a Kowloon release.
- Citizenship has three requirements: ownership (section 6), calm (the deep breath, section 1) and clear boundaries (sections 2 and 6).
- An **area** is one capability together with the modules it reaches. The maintainer closes it when its slice is done (section 7). The pilot is not an area.

## 1. The deep breath

The deep breath is the pause in feature and fix work while the first area is aligned. During it, the only work is the alignment process of section 7 and the exception below.

- Do not start, continue or finish any feature, fix, refactor, test, document or plan item.
- `alignment/in-flight.md` lists what was unfinished when the deep breath began. Read it before acting. If you hold unfinished work that is not listed, add a line and tell the maintainer. Section 7 holds its format, and the rules for other machines, running jobs and open issues.

**Exception: wrong results.** An issue of the wrong-result class is not frozen. It is fixed under the repository's existing rules for a fix. Such a fix may change source, tests and documents, and the stop of section 13 about changes outside `alignment/` does not apply to it. Every other limit still does.

**When it ends.** The deep breath ends when the maintainer has closed the first area and says that work may resume. Afterwards, work in a closed area follows section 17. In an area not yet closed, section 2 still holds: a change there is made only at the maintainer's specific request, and removes nothing. Alignment of the remaining areas continues, one at a time.

## 2. Status quo holds

Until an area has been aligned and the maintainer has closed it, everything that exists stays as it is.

- **Nothing is dropped until the maintainer names it.** Do not delete, rewrite, prune, move or disable any file, rule, memory entry, context file or log that existed before alignment began. A rule or file is retired only after its area is closed, by a signed closing statement or release report that names the removal. Silence, inference and "it looks obsolete" are not confirmation. The working records this process creates under `alignment/` are not status quo; section 7 says when they end.
- **Existing rules keep applying.** Follow the instruction files as your working practice, except where they would have you do work that section 1 pauses or section 6 forbids.
- **Existing rules are potential intent.** Each is a candidate for the maintainer to confirm. Record it; do not treat it as confirmed.

## 6. Roles and limits

- **Maintainer:** the repository owner. Decides what is intent, writes and signs every change under `spec/`, and closes areas.
- **Tutor:** answers for derived work: what it is for and how it links to its source.
- **Agent (you):** extract, cross-reference, draft candidates and proposals, generate questions, run scripts, report.

At all times, during alignment and after it, you may not:

- Create, edit or delete anything under `spec/`, the real `.gitattributes`, or `BINDINGS.md`, or word a spec item for the maintainer. Intent is written by the maintainer, through the intent cycle of section 3.
- Create, change or remove instruction files, agent definitions, settings, hooks, memory files or CI configuration, unless the maintainer asks for that specific change.
- Sign anything, or use any signing key or agent the session can reach. If your environment signs commits automatically, say so and stop.
- Record the maintainer's own statements of intent in memory, notes or logs. Extraction, and the blind re-extraction of a closed area, depend on your not having seen them. A decision about the method (section 14) is not a statement of intent.
- Decide whether a difference between code and intent is a bug or a spec error.

In alignment work you also may not:

- Change source, tests or documents. No fixes, no refactoring, no clean-up, however obvious.

When unsure whether an action is allowed, do not take it. Ask.

## 13. Stop and ask when

- A status-quo rule conflicts with this file or a file under `method/`, or with another status-quo rule. Do not pick one.
- You find secrets, credentials or third-party code with unclear licensing.
- A step would require changing anything outside `alignment/`.
- You cannot tell which class a path belongs to.
- You have built on your own earlier output for more than one step without the maintainer reviewing it.
- A deep breath you relayed to another machine has not been confirmed there.
- A change you are asked to make in a closed area would alter what a signed item says, or behaviour that no item mentions.

## 16. Commits

- An alignment commit holds only files under `alignment/`, and one step.
- A change the maintainer asks for elsewhere, and a fix under the exception of section 1, are separate commits under the repository's existing rules.
- Every commit an agent helped write ends with `Assisted-by: <agent and version>`, in place of any other attribution trailer.
- A commit carries `Tutor: <name>` only when the tutor reviewed or answered for what it contains.

## 17. After an area is closed

Closing an area makes your obligations there permanent.

- **Signed intent does not expire.** In a closed area the signed items under `spec/` are the intent, in every later session, until the maintainer signs a change to them.
- **The method binds you with or without a gate.** The gate of section 11 is a second line. Where it is not written, not running, or cannot see a change, you follow the method all the same.
- **Read `method/changes.md` before any change in a closed area.** It says what counts as a change of intent and what you may not weaken.
- **The repository is aligned when the maintainer says so.** One signed statement names the closed areas and the paths left outside on purpose. Until then, section 2 holds for every path that no closed area covers.
