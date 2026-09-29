# Consumer briefing — 2026-09 the `sv` property verbs' JSON is schema-pinned now

> **Audience:** rosf. This is the half of your ask that the `--json` flag did not deliver.
>
> **Related:** closes [mununu#541](https://github.com/Mumunu-team/mununu/issues/541).
>
> **TL;DR:** **no output change** — the documents are byte-identical for `verify-recoverability`, and the field sets are unchanged for all three. What changed is that they now come from Rust types, are published under `docs/api-schemas/`, and are guarded by the drift detector that already covers `sv verify-auto`. You asked for "the schema-pinned document, never the transcript"; this is the pinning.

## What you get

| File | Command |
|---|---|
| `sv-verify-recoverability-report.schema.json` | `sv verify-recoverability --json` |
| `sv-verify-liveness-report.schema.json` | `sv verify-liveness --json` |
| `sv-verify-liveness-all-report.schema.json` | `sv verify-liveness-all --json` |

Draft-07, derived from the Rust types via `schemars`, same pipeline as `sv-verify-auto-response.schema.json`. **A field added to the struct without regenerating the schema fails CI** — that guard is the substance here, not the files.

```json
// sv verify-recoverability --json
{ "file": "design.sv", "property": "AG EF (state == 0)", "verdict": "unknown" }

// sv verify-liveness --json
{ "file": "design.sv", "property": "AG((req == 1) -> AF (ack == 1))",
  "verdict": "holds", "decided_by": ["native-bmc", "spacer"] }

// sv verify-liveness-all --json
{ "file": "design.sv", "property": "...", "verdict": "holds",
  "responses": [ { "response": "req == 1 => ack == 1", "decided_by": ["spacer"] } ] }
```

## What changes for you: nothing you have to do

The documents are the same. `sv verify-recoverability`'s output is **byte-identical** to the previous release, verified in the `mununu-sva` image against the same design.

One cosmetic difference worth knowing before you diff two releases: for `verify-liveness` and `-all`, **JSON key order** may differ. The old inline literals serialised through `serde_json::Value`, which sorts keys alphabetically; a typed struct emits declaration order. Key order is not semantic in JSON and every conformant parser ignores it — this is noted so a whitespace-level diff does not read as a change.

**Field sets are unchanged.** Nothing renamed, nothing dropped.

## What is deliberately not pinned

**`refinement`** (present under `--refine` / `--config-values` / `--discover-assumptions`) is typed as free-form. `VerdictRefinement` is its own evolving surface, and pinning its internals here would couple two schemas that change for different reasons — you would get drift failures on this schema caused by unrelated refinement work.

**The error envelope.** Under `--json` a failure is `{verb, file, error}`, and that shape is intentionally not schema-pinned: it is a failure envelope rather than a report, and pinning it would invite branching on its shape instead of on the exit code. Branch on exit code; read `error` for the human reason.

## mununu#541 is now closed

All four parts:

| part | shipped |
|---|---|
| `context synth --dump-json` writes the file, 19 silently-ignored flags refuse | `62d9c6a` |
| `--json` accepted on the three `sv` property verbs; errors are JSON too | `a3343f5` |
| **the three reports are typed + schema-pinned + drift-guarded** | **this change** |
| `sv verify` returning a BTOR2-shared type | see below |

**The one thing we did not do**, and it is a deliberate call rather than an omission: `sv verify` still returns `Btor2VerifyResponse`, a type shared with the pure-BTOR2 verbs and therefore missing SV-specific lift information. That is a *response-shape* change on an HTTP surface with existing consumers, not a CLI report addition, and it deserves its own issue with its own migration note rather than riding along here. Tell us if it blocks you and it gets filed with your case attached.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | none — no toolchain or dependency change | **No** |
| `mununu-sva` | none | **No** |
| `mununu-sva-pono` | none | **No** |
| `hw-verif` | none — not a mununu image | **No** |

## Not covered here

- **Schemas for the `btor2` peers of these verbs.** Those already have request/response schemas; the CLI-report shapes above are the new ones.
- **A stability guarantee across major versions.** The drift detector catches an *accidental* change; an intentional one still ships with a briefing.

---

**Provenance.** Issue: [mununu#541](https://github.com/Mumunu-team/mununu/issues/541), filed from rosf. Output verified unchanged in the `mununu-sva` image. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
