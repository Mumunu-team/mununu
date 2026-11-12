# Consumer briefing: immediate `assert` / `assume` / `cover` statements are now reported, not dropped silently (2026-11)

> Source of truth: [`ImmediateAssertionSite`](../../crates/mununu-core/src/adapter/slang/translate.rs) — surface: CLI+API

**Audience:** anyone who runs `sv verify-auto`, `sv extract-sva` or the SV translate path (loading a `.sv` file on the CLI, or `POST /api/v1/context/import` with SV content) on designs that write an `assert (…)`, `assume (…)` or `cover (…)` inside an `always` block, and who reads the report on the CLI or over the HTTP API.

## TL;DR

- **The defect.** The lift reads the design with `read_slang --ignore-assertions`, which drops every immediate assertion. The SVA extraction reads only concurrent ones (`assert property`, …). So an immediate statement was neither checked nor applied, and **nothing in the report said so**.
- **Why an `assume` matters most.** A dropped immediate `assume` leaves the model allowing behaviour the author excluded. The model then over-approximates the assumed environment:
  - a universal `holds` (an `assert property`, `AG`, `nu`) still holds under it;
  - a `violated` may be a counterexample that breaks the dropped assume;
  - an existential or branching `holds` (a cover, `EF`, `AG EF`) does not transfer.
- **Now:**
  - **`sv verify-auto`**:
    - each immediate `assume` is listed in `assumptions.unapplied` with the reason, in every `--assumes` mode;
    - a `dropped-immediate-assume` note (level `soundness-caveat`) states the direction above;
    - a `dropped-immediate-assertion` note (level `scope-caveat`) lists the immediate `assert` and `cover` statements nobody checked.
    - Each item names the statement as `assume m.sv:4`, or `assert a_imm (m.sv:6)` when it has a label. The line comes from the source-index run; when that run is not available, the item says `in <scope>` instead.
  - **`sv extract-sva`**: a new `immediate_assertions` array (`kind`, `scope`, `label`, `at`) on the CLI's `--json` output and in the API response. The text output lists the statements.
  - **The SV translate path**: one `UnsupportedConstruct` warning per run that names each statement's keyword and line. The count comes from a token scan of the source text you passed, so it does not cover `include`d files.
- **Verdicts do not change.** Only the report says more. A design with an immediate `assume` and a `violated` row now carries the note that tells you the counterexample may break it.
- **JSON shape:**
  - `sv extract-sva` gains one field;
  - `sv verify-auto` gains two note kinds and new entries in an existing list;
  - the translate path's `warnings` gains entries.
  
  No field is removed or renamed.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Designs with immediate assumes gain a soundness-caveat note and `assumptions.unapplied` entries. A gate that treats every soundness-caveat note as blocking now blocks on them. | Rewrite each immediate `assume` as `assume property` to have it applied. Or accept the note. |
| **monono** | Same, if any harness writes immediate assumes. | Same. |
| **mununu-ui** | The translate response's `warnings` may carry the new line. | None; warnings are shown as text. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Origin:** mununu#1001 (the dropped `assume`) and mununu#988 (the dropped `assert`).
- **Tests:**
  - `every_immediate_assertion_is_recorded_once_per_instance`: a real slang dump captured in the `mununu-sva` image. It counts each instance of a module once, skips a generate branch the elaboration does not take, and checks that the source-index walk lists the same statements in the same order.
  - `an_immediate_assertion_is_reported_as_unchecked_and_an_immediate_assume_as_unapplied`: the planner in every mode, the notes, and the line join.
  - `immediate_assertions_are_found_by_keyword_and_line`: the translate path's scan.
  - `e2e_immediate_assertions_are_reported_not_dropped_silently`: #1001's design end to end in `mununu-sva`.

## Not covered here

- **Applying an immediate `assume`** as a precondition, which is #1001's option (a), is not implemented. The statement is reported, not applied. Rewrite it as `assume property`.
- **The other SV lift verbs** (`sv cegar`, `sv mutate`, on the CLI and the API) return a BTOR2 model with no warnings channel. They drop immediate assertions in the same way and do not yet say so.
