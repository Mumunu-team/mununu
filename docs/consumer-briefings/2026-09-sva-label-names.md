# Consumer briefing — 2026-09 a property can be named by its SVA **label**, not just its index

> **Audience:** monono (reported it — ask 22), ROSF, and any consumer that pins `--expect` or reads `properties[].name`.
>
> **Provenance:** [mununu#544](https://github.com/Mumunu-team/mununu/issues/544). Builds on [#537](https://github.com/Mumunu-team/mununu/issues/537) (`--expect*`) and [#536](https://github.com/Mumunu-team/mununu/issues/536) (the typed report). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## TL;DR

`properties[]` now carries **`label`** — the assertion's SV label — alongside the positional `name`. `--expect NAME=VERDICT` accepts **either**, label first.

**Nothing breaks.** Existing index-based pins keep resolving, deliberately, so you can migrate property-by-property instead of in one cut. The only additive change to the wire format is one optional field.

## What was wrong

`--expect` took the **positional** name. monono measured, on one run:

| invocation | exit |
|---|---|
| `--expect "a_reseed_only_at_a_span_edge=holds"` | **4** |
| `--expect "affine_spr_sva_sva_0=holds"` | **0** |

So the index was load-bearing in a CI surface. Inserting a property mid-file silently re-points every later name — and the pin still *resolves*, to a different property. It happened to monono, and their gate caught it **by luck**: the newly-named property happened to hold in the twin.

`--expect-count` (which they now pin for all 23 arities) catches the insertion case, because an insertion changes the count. It does not catch a property **replaced in place**: same count, different meaning.

### Two of our own records were wrong, and one of them caused this

1. **`docs/api-schemas/verdict.md` documented `name` as "SVA label from the source".** It never was. A consumer reading that would reasonably pin to the label and expect it to resolve — which is exactly what happened. **Corrected in this change.**
2. **`translate.rs` said the label was unavailable** — *"slang's `--ast-json` does not attach the SV label to the assertion node — the label lives on a separate symbol-table entry."* The second half is wrong, and it made this ask look expensive. The label needs no symbol table: it is inline on the **enclosing `Block`** node, as `"<address> <label>"`. Verified against our own checked-in fixtures:

```text
ProceduralBlock → body: Block { block: "6338700059712 ap_bool", body: ConcurrentAssertion }
```

The `ConcurrentAssertion` node carries only `assertionKind` / `ifTrue` / `kind` / `propertySpec` — which is why it was believed absent. It is one level up.

## What changed

- `properties[].label` — the SV label, when the assertion has one. `null`/absent otherwise.
- `properties[].name` — unchanged, still `<module>_sva_<index>`.
- `--expect NAME=VERDICT` resolves `NAME` against the **label first**, then the positional name.
- A **rename** is now a hard error instead of a silent re-point; a **reorder** is a no-op.

## What to update, per consumer

### monono

- **Migrate your 23 pins to labels, at whatever pace suits.** Both spellings resolve, so a half-migrated set is fine. Once a property is pinned by label, an insertion or reorder cannot re-point it.
- **Keep `--expect-count`.** It still catches a property that stopped being produced entirely, which no per-property pin can see.
- **Residual gap, stated rather than papered over:** `@mununu_guarantee` properties have **no label** and stay positional (`ann_guarantee_<index>`), because `MununuAnnotation` carries only `tag`, `value` and `source_line` — no name field. If you pin annotation-derived properties, they remain index-fragile. Giving the annotation an optional name is a separate surface change; say the word and it gets an issue.

### ROSF

- The lane reads `properties[]` from the schema-pinned document, so **no code change is required** — the new field is optional and additive.
- Worth adopting anyway: rosf's `[[queries]]` name properties, and binding those to labels rather than indices makes a corpus manifest robust against an upstream RTL edit.

### Report-parsing impact

**Additive only.** One new optional field (`label`), `skip_serializing_if = "Option::is_none"`, so a report with no labelled assertions is byte-identical to before. No field renamed, removed, or retyped. No verdict value changes. The JSON schema is regenerated in this change (`docs/api-schemas/sv-verify-auto-response.schema.json`) and the drift detector passes.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | new `label` field + `--expect` accepts labels | **Yes** |
| `mununu-dev` | test/lint image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; this is the SVA path | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu; rebuild only when adopting labels | Optional |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only | No |

## Test the transition

```bash
# 1. The label is recovered, and the positional name still works.
cargo test -p mununu-core --lib -- adapter::slang::translate::tests::an_assertions_sv_label \
  adapter::slang::translate::tests::an_unlabelled \
  adapter::slang::expectations::tests::a_claim_can_name \
  adapter::slang::expectations::tests::a_claim_by_positional \
  adapter::slang::expectations::tests::a_renamed_label

# 2. On your own corpus: the label now appears, and BOTH spellings resolve.
mununu sv verify-auto <design> --frontend slang --json | jq '.properties[] | {name, label}'
mununu sv verify-auto <design> --frontend slang --expect "<your_label>=holds"   # exit 0
```

## Not covered here

- **`@mununu_guarantee` labels** — the residual gap above. No issue yet; ask if you want one.
- **[#543](https://github.com/Mumunu-team/mununu/issues/543)** — the exit-134 triage. Open.
- **[#545](https://github.com/Mumunu-team/mununu/issues/545)** — a cutpoint that frees an array read's value but keeps its timing. Open; design work.
- **[#541](https://github.com/Mumunu-team/mununu/issues/541)** — extending the typed report to `verify-recoverability` / `verify-liveness` / `context synth`. The `label` field lives in the shared shape, so those three inherit it when they land.
