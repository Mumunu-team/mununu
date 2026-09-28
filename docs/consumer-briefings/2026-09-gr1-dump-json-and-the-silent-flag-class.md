# Consumer briefing — 2026-09 `context synth --dump-json` now writes the file, and 19 silently-ignored flags now fail loudly

> **Audience:** rosf, which consumes mununu as a subprocess and measured this. monono, if you script `context synth`.
>
> **Related:** closes the `context synth` half of [mununu#541](https://github.com/Mumunu-team/mununu/issues/541). The two remaining verbs (`sv verify-recoverability`, `sv verify-liveness`) are still open on it.
>
> **TL;DR:** `--dump-json <FILE>` on `--controller-mode gr1` **writes the file** — it was accepted, exited 0, and wrote nothing. **⚠️ BREAKING, deliberately:** 19 other flags that this path silently ignored are now **errors with exit 1**, and one of them (`--counterexample`) is in the command your issue used. Drop it and the run succeeds.

## The defect you measured

```console
$ mununu --quiet context synth examples/tlsf/request_grant.tlsf --adapter tlsf \
    --controller-mode gr1 --automaton placeholder --counterexample \
    --dump-json /tmp/d1.json
GR(1) controller synthesis (examples/tlsf/request_grant.tlsf):
  Realizable: yes
$ echo $?        # 0
$ ls /tmp/d1.json
ls: cannot access '/tmp/d1.json': No such file or directory
```

Fixed. The same command, minus `--counterexample`, now ends:

```
  Controller synthesized (55 lines of SV); pass --emit-sv <FILE> to write it
  JSON report written to /tmp/d1.json
```

```json
{
  "verb": "context-synth",
  "controller_mode": "gr1",
  "source": "examples/tlsf/request_grant.tlsf",
  "realizable": true,
  "game": { "states": 13, "monitor_bits": 2 },
  "controller": { "emitted": true, "sv_lines": 55, "written_to": null },
  "notes": []
}
```

`written_to` is the `--emit-sv` path when you passed one, `null` otherwise — so one document tells you whether a controller exists *and* whether it was persisted.

## ⚠️ The class was 4× larger than the issue said

You named 5 ignored flags. Measured against `ContextSynthesizeArgs`, the gr1 path reads only `context`, `adapter` and `emit_sv` — it silently ignored **21**. The full set is now decided, one flag at a time:

| disposition | flags |
|---|---|
| **honoured** | `--adapter` · `--controller-mode` · `--emit-sv` · `--dump-json` |
| **refused, exit 1, with a reason** | `--sidecar` · `--mode` · `--preprocessor` · `--formula` · `--template` · `--template-arg` · `--no-partitions` · `--minimize` · `--counterexample` · `--deadlock-traces` · `--max-counter-traces` · `--no-proof-obligations` · `--emit-dsl` · `--dump-diagnostics` · `--print-structure` · `--print-ctxdsl` · `--output-format` · `--emit-native` · `--soundness-report` |
| **warned, not refused** | `--automaton` |

Each refusal names what to use instead rather than just saying "unsupported":

```
--controller-mode gr1 cannot honour the following flag(s). They were previously accepted and
silently ignored, which is why this is now an error:
  --counterexample — gr1 reports realizability; an unrealizability WITNESS is
                     `btor2 game --objective recurrence`'s stall lasso, not this verb
```

**Action for rosf: drop `--counterexample` from the invocation in your issue.** It never did anything; now it says so. If any of the other 18 appear in your scripts, the error names the replacement.

**`--automaton` is warned rather than refused, and the reason is structural, not a judgement call:** clap makes it *required* for this subcommand, so refusing it would reject the documented invocation. You still get a visible line saying it is ignored on this path.

## The durable half: a guard so this class cannot recur silently

The fix that matters is not the 19 refusals — it is that adding a flag to `context synth` without deciding whether gr1 honours it now **fails a test on the author's machine**:

```
these `context synth` flags are neither honoured nor refused on the gr1 path, so they are
SILENTLY IGNORED — decide for each one: ["--new-flag"]
```

The list is hand-written rather than derived, on purpose: the point is that a human adding a field has to come and choose. The test also requires each refusal to carry a real reason — it caught three of ours that said only *"see --counterexample"* and *"same reason as --formula"*, which is exactly the unhelpful shape it exists to prevent.

This is the same "check that cannot fail" family as mununu#499 and rosf#37, arriving in argument parsing.

## Still open on mununu#541

Your other two blocked objectives are **not** in this change:

| verb | structured output |
|---|---|
| `sv verify-auto` | `--json`, schema-pinned (#536) |
| **`sv verify-recoverability`** | **still none** |
| **`sv verify-liveness`** | **still none** |
| `context synth --controller-mode gr1` | **`--dump-json`, this change** |

Those two are new typed report surfaces with CLI + API parity and a schema, not a flag fix, so they land separately. #541 stays open for them.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | none — no toolchain or dependency change | **No** |
| `mununu-sva` | none | **No** |
| `mununu-sva-pono` | none | **No** |
| `hw-verif` | none — not a mununu image | **No** |

A consumer pinning a mununu commit needs the new commit.

## Not covered here

- **A JSON schema for the synthesis report.** `sv verify-auto`'s response is schema-pinned with a drift detector; this document is not yet. It ships with the two verbs above so one schema decision covers all three.
- **The projection path.** It already honoured these flags and is unchanged.
- **mununu#548's O-2**, the per-property note join — also waiting on the same wire-format decision.

---

**Provenance.** Issue: [mununu#541](https://github.com/Mumunu-team/mununu/issues/541), filed from rosf. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
