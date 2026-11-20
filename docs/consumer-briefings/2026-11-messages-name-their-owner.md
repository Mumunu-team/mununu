# Consumer briefing: a message that reports a gap says whose it is (2026-11)

> Source of truth: [`adapter::owner`](../../crates/mununu-core/src/adapter/owner.rs) (`mununu_defect`, `mununu_limitation`, `technique_limit`) and the sites that call them — surface: CLI+API — the text of refusals, abstentions, `bottom_reason.detail`, notes, `Skipped` reasons, adapter warnings and `500` bodies across the adapters, the exact and cube engines, the GR(1) builder, the verify orchestrator and the API

**Audience:** anyone who reads or matches on the text of a refusal, abstention or warning.

## TL;DR

- **Message text only (mununu#1121, U2.10).** About a hundred messages reported an impossibility or a gap and stopped there ("unsupported", "not yet supported", "internal error", "should not happen", "cannot"). Each now ends with **who owns it**, in one of three fixed phrasings:
  - *"— a limit of the technique, not a budget: <what helps>"* (a cap or a shape the engine cannot cross; the other engine or the reshaping that does);
  - *"— not supported by mununu yet (a mununu limitation; nothing to change in the design)"*, with *"; instead: …"* when something the user can do today exists;
  - *"— a mununu defect, not something the input can change: please report it, with the input, at <the issue URL>"*.
  The head of each message (what happened, in the input's names) and any candidate list or remedy it already carried are unchanged; roadmap step names and tier tags that stood in for the reason are gone from these messages.
- **No verdict, kind, field or JSON shape change.** `AdapterWarning` / `AdapterError` kinds, `bottom_reason.kind` values and note kinds are unchanged; only the strings read differently. The API's `500` bodies for a rendering failure share one sentence through `mununu_defect`. A consumer that matched one of the old tails must update the match.
- Two engine seams changed shape internally, not on the wire: the IC3 pass's cube enumeration tells a cap from a solver timeout, and the exact two-player entry points name which of three things failed (the BTOR2, the atom, or the engine's own abstention) instead of a folded sentence.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Refusal / abstention / warning text ends with the owner clause; kinds unchanged. | Nothing, unless you matched an old tail (no match found in the repository). A gate may now branch on the three phrasings. |
| **monono** | Same. | Same. |
| **mununu-ui** | Error strings shown verbatim read differently. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the text. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the remedy-message audit of 2026-11-19 (Track U2 row 10, mununu#1121; the per-site table with each site's owner is on the issue).
- **Tests:** `x1121_unknown_adapter_message_names_its_owner_and_the_adapters_verify_dispatches` (the reproducer: the orchestrator's unknown-adapter refusal names the owner and lists the adapters it dispatches; compiles on the parent and fails there), the three helper tests in `owner.rs`, and a seam test tying the dispatched-adapter list to the dispatch.

## Not covered here

- Three doc passages quote an old tail as an example (`docs/verifying-rtl.md` on "unsupported binary op", `docs/abstraction.md` on RVWMO); they describe the head, which is unchanged, and are refreshed with U2.7.
- U2.7 (Rust identifiers, roadmap steps and tier names in user text) is the next row and reuses these helpers.
