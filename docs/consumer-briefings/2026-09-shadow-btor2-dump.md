# Consumer briefing — 2026-09 you can now hand us the model the engine actually ran

> **Audience:** monono, and anyone who has been asked for a reproducer and could only send the lift.
>
> **Related:** closes [mununu#552](https://github.com/Mumunu-team/mununu/issues/552), which you filed while trying to give us a reproducer for [#543](https://github.com/Mumunu-team/mununu/issues/543).
>
> **TL;DR:** `MUNUNU_SHADOW_BTOR2_DUMP=<path>` writes the **post-`$past`-shadow** BTOR2 with every property's mu-formula alongside it, in one file that still parses as BTOR2. Additive, diagnostic-only, no verdict changes. **⚠️ The variable is not the name the issue proposed** — see below.

## What was missing, in your words

> `spr_cost.design.btor` is post-lift but PRE-shadow, since `augment_with_past_shadows` runs inside
> mununu. If you want the post-shadow model I do not have a way to emit it from the CLI.

There wasn't one. `MUNUNU_KEEP_YOSYS_TMP=1` and `sv emit-btor2-per-module` both stop at the lift.

For a `$past`-bearing property that is exactly where the two models diverge — **the shadow chain is
the part that widens the cone**. Reproducing #543 in-house meant hand-rebuilding the augmented
model from a guessed base list, and that reconstruction could never settle the question that
mattered: whether it matched the real lift. It cost two definite answers, once on each side.

## Using it

```bash
MUNUNU_SHADOW_BTOR2_DUMP=/tmp/repro.btor2 mununu sv verify-auto design.sv
```

One file per run. The header is all `;` comments, so **the dump is still valid BTOR2** — feed it
straight to `btor2 verify`, or attach it to an issue as-is.

```
; mununu#552 — the BTOR2 the engine runs, dumped after model preparation.
; INCLUDES: the SV lift, `$past` shadow registers, reset pinning,
;           `--config-value` pins, and any `sv mutate` fault.
; EXCLUDES: antecedent shadow synthesis (`_mununu_antshadow_*`), which runs
;           LATER and PER-PROPERTY inside the engine. …
;
; Properties, as the engine parses them (post `--config-value` substitution).
; Shadow atoms appear here as `<base>__past`; bit-slices as `<base>__bits<hi>_<lo>`.
; spr_cost_sva_0: nu X. ((tog_dst_q == tog_dst_d_q) …) …
1 sort bitvec 32
…
```

**The formulas are the other half of a reproducer.** The model alone doesn't tell you what was
asked of it, and the shadow and slice atom names (`<base>__past`, `<base>__bits<hi>_<lo>`) are
exactly the ones you cannot reconstruct by inspection. Both halves in one file means the handover
is one attachment.

## ⚠️ It is NOT called `MUNUNU_KEEP_SHADOW_BTOR2`

The issue proposed that name and we did not use it, deliberately. Both existing `MUNUNU_KEEP_*`
variables are **boolean keep-the-tempdir flags**:

| variable | takes |
|---|---|
| `MUNUNU_KEEP_YOSYS_TMP` | `1` / `true` — keep the directory |
| `MUNUNU_KEEP_VERILATOR_TMP` | `1` / `true` — keep the directory |
| **`MUNUNU_SHADOW_BTOR2_DUMP`** | **a path** |

The path-valued precedents are `MUNUNU_INTERP_DUMP` and `MUNUNU_SPCR_OUT_DIR`. Borrowing the
`KEEP_` prefix would have named a convention this does not follow, and you would have reasonably
tried `=1` first.

## What it deliberately does NOT contain

**Antecedent shadow synthesis.** `_mununu_antshadow_<N>` registers are synthesised *per property*,
*inside the engine*, after this dump is written. If you are reproducing an `A |=> C` with an
input-derived antecedent, that rewrite is not in the file — and the header says so rather than
leaving you to discover it.

That omission is stated in the artifact itself on purpose. A dump that silently dropped a rewrite
would be the same class of defect this issue exists to close: an artifact that looks complete and
is not.

## It cannot fail your run

Diagnostic-only, mirroring `MUNUNU_VERIFY_AUTO_PARTIAL_JSON`:

- unset → no dump, no cost;
- an **empty or whitespace** value is treated as unset, not as the path `""`;
- an unwritable path **warns once** and the run continues.

A diagnostic that can abort the run it is diagnosing is worse than no diagnostic. There is a test
pinning each of those three.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | none — no toolchain or dependency change | **No** |
| `mununu-sva` | none | **No** |
| `mununu-sva-pono` | none | **No** |
| `hw-verif` | none — not a mununu image | **No** |

## Not covered here

- **Per-property dumps.** The shadow set is computed per *design* from every property's bases, so
  one file per run matches the lift's own granularity — which is the distinction you raised from
  the other side ("the lift is per-DESIGN, not per-property").
- **Counterexamples and notes.** The dump carries the model and the formulas. The full report is
  still the report.

---

**Provenance.** Issue: [mununu#552](https://github.com/Mumunu-team/mununu/issues/552). Policy:
[`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
