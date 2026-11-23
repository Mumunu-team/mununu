# Consumer briefing: seven messages lose a run of spaces in mid-sentence (2026-11)

> Source of truth: [`dropped_immediate_notes`](../../crates/mununu-core/src/adapter/slang/verify_auto_text.rs) — surface: CLI+API — `mununu sv verify-auto` and `POST /api/v1/sv/verify-auto`

**Audience:** anyone who compares the text of a `sv verify-auto` note, a bottom reason or an `--expect` error with a stored copy.

## TL;DR

- **Seven messages carried a run of about twenty spaces in the middle of a sentence (mununu#1209).** Each is a string written over several source lines; the line continuation had been lost, so the indentation of the next source line was part of the text. Each run is now one space. No word changes.
- **The messages:**

  | message | where it is read |
  |---|---|
  | the `detail` of the `dropped-immediate-assertion` note | `notes[].detail`, the text report |
  | the `detail` of the `dropped-immediate-assume` note | the same |
  | the `detail` of the `time-budget-exceeded` note | the same |
  | the one-line reason for `no-state-model-non-safety` | the CLI's reason line |
  | the one-line reason for `unclassified` | the same |
  | the error for a verdict `--expect` does not know | the CLI's stderr |
  | the error for a verdict `expectations.named` does not know | the API's error body |

- **No verdict changes.** No kind, level, item, field or exit code changes.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | The seven texts above, if you store or compare one whole. | Compare on `kind`, not on `detail`. If you keep a copy of a text, refresh it. |
| **monono** | the same | the same |
| **mununu-ui** | no UI surface for `sv verify-auto` | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Only to get the corrected texts. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image, and only to get the corrected texts. | They consume the binary. |

## Provenance

- **Found by:** reading the source during the review of mununu#1208.
- **Class:** `source-layout-in-message` ([`docs/defect-classes.md`](../defect-classes.md)).
- **Tests:** `no_message_keeps_the_indentation_of_a_wrapped_source_line` and its control `the_scan_tells_a_lost_continuation_from_a_line_the_program_indents` (`crates/mununu-core/tests/policy`). The gate reads the source of every crate, so it does not depend on the wording of any message.

## Not covered here

- A lost continuation whose line break was kept (the message would break in mid-sentence). No message has that shape today, and the gate does not look for it: mununu#1220.
