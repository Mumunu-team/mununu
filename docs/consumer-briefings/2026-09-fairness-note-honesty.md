# Consumer briefing — 2026-09 `@mununu_assume GF` note text now points at the supported route

> **Audience:** ROSF (API consumer via `--profile industrial`), monono (direct CLI + API consumer), any orchestrator that parses the `sv verify-auto` `annotation-properties` verification note.
>
> **Related:** [mununu#477](https://github.com/Mumunu-team/mununu/issues/477) — the ticket that surfaced the misleading framing.
>
> **TL;DR:** the `annotation-properties` note text on the `sv verify-auto` response has been rewritten. It no longer reads as "the tool cannot do fairness-constrained model checking"; it points users at the routes that ARE shipped (`mununu btor2 game --objective recurrence` and CTXDSL's GR(1) engine). Same note kind, same level, same items list — **only the `detail` string changed.** No wire-format shape change; a consumer that only inspects `verdict` / `kind` / `level` is unaffected. Two doc pages were also updated (`Composition.md` gains a composition footgun gotcha; `Property-Templates.md` clarifies where fairness IS supported).

## What changed

- **`crates/mununu-core/src/adapter/slang/verify_auto.rs`** — the `annotation_note` detail string at line ~1346 rewritten. Old text ended "and is recorded here" after saying "no native fairness-constrained model checking"; new text says fairness IS shipped via `mununu btor2 game --objective recurrence` (single-pair GR(1) on the emitted BTOR2) and via CTXDSL's GR(1) game engine, with a `mununu#477` reference for the SV-path auto-routing bridge.
- **Struct doc comment** at `AnnotationScan::assumes` (line ~1234) mirrors the same correction.
- **`wiki/Composition.md`** — new "Key Gotchas" bullet: *a shared label the component cannot take is a label the environment cannot emit — silently.* Filed against a real monono debug per the ticket comment.
- **`wiki/Property-Templates.md`** — the "Soundness default" callout about `AF` / `GF` fairness updated to name where fairness IS supported today (`btor2 game --objective recurrence` and CTXDSL GR(1)), rather than the flat "not assumable" statement.
- **Plan doc** `/.claude/plans/477-fairness-assumption-sv-bridge.md` (agent-side, not in-repo) — the Option B follow-up bridging `sv verify-auto` to the game engine, scoped as a future track.

## What did NOT change

- Wire format — unchanged. The `annotation-properties` note's `kind`, `level`, and `items` array are byte-identical.
- Response shape / route table / engine behaviour — unchanged.
- The `sv verify-auto` schema shipped in mununu#485 — unchanged.
- CLI / API surface — no new flags, no new endpoints.

## For ROSF and monono

**What to update:** nothing forced. A consumer that surfaces the annotation-properties note to a user or log will see the new text; verify any regex/keyword matches on the old string content ("no native fairness-constrained model checking", "recorded here") and remove or update them.

**What to expect on already-affected properties:** the `wb_mem_client` case from the ticket comment — `AG(req → AF ack)` under `@mununu_assume GF grant_cpu` — still returns `Unknown` on the `sv verify-auto` path (Option B is what fixes that). This PR does NOT change the verdict; it changes the **note text** that told users the tool couldn't do fairness at all. The actual capability IS shipped, just via a different verb — walk it through `mununu sv emit-btor2 && mununu btor2 game --objective recurrence` (or model in CTXDSL).

**Docker rebuild:** none required.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod) | binary picks up the rewritten note detail; no behavioural change | No — unless you pin a tag and want the new text |
| mununu `Dockerfile.dev` | binary bump | Only if the dev workflow requires the new binary |
| mununu `Dockerfile.sva` | binary bump; no e2e behaviour change | No |
| mununu `Dockerfile.extract`, `.extract-*` | no impact | No |
| rosf `Dockerfile` / `Dockerfile.dev` / `.hw` | consumes mununu CLI/API; note-text change is user-visible if surfaced | No — behaviour updates on next binary pull |
| monono Docker (if any) | primary beneficiary of the corrected framing | No — pull the new binary when adopting |
| mununu-ui deployment | no impact | No |

## Verification steps

- `mununu sv verify-auto <sv-with-@mununu_assume>` returns the note; inspect `verification_notes[i].detail` for the new text.
- `cargo test -p mununu-core --lib h5_gr1_assume_recorded_and_empty_source_has_no_note` passes.
- `docs/api-schemas/sv-verify-auto-response.schema.json` unchanged (no derive touched).

## Provenance

- Fix commit: (pending merge — branch `fix/477-annotation-note-fairness-honesty`).
- Ticket: [mununu#477](https://github.com/Mumunu-team/mununu/issues/477).
- Follow-up track: agent-side plan `477-fairness-assumption-sv-bridge.md` — the Option B full bridge; not scoped for this PR.
- Policy: [`../policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here (follow-ups)

- **The SV verify-auto → game-engine bridge.** Detecting `GF <atom>` bodies in the annotation scanner, dispatching to the recurrence game engine automatically, and folding the definite verdict back into the property row. Planned as Option B on the same ticket; agent-side plan doc names the phases + effort estimate.
- **Multi-pair GR(1) on the SV path.** The CTXDSL path supports it; the SV bridge starts single-pair per the ticket's narrowed ask.
- **General LTL assumptions.** The ticket explicitly narrows to `GF <signal>` conjunctions on primary inputs.
