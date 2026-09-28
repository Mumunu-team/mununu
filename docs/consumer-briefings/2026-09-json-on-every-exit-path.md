# Consumer briefing — 2026-09 `--json` works on the `sv` property verbs, and an error is a document now

> **Audience:** rosf, which consumes mununu as a subprocess. monono, if you script these verbs.
>
> **Related:** advances [mununu#541](https://github.com/Mumunu-team/mununu/issues/541) — the `context synth` half shipped in `62d9c6a`. **#541 stays open** for the schema pinning.
>
> **TL;DR:** additive, no verdict changes. `--json` is now accepted on `sv verify-recoverability`, `sv verify-liveness` and `sv verify-liveness-all` — it used to fail argument parsing with **exit 2**. With it, **every exit path emits one JSON document, errors included**; without it, behaviour is unchanged.

## ⚠️ First, a correction to the issue

mununu#541's table says these verbs have **"structured output: none"**. Measured, that is not what a run does:

```console
$ mununu --quiet sv verify-recoverability gated_domain.sv --target "state == 0"
{
  "file": "examples/clock_gating/gated_domain.sv",
  "property": "AG EF (state == 0)",
  "verdict": "unknown"
}
$ echo $?    # 0
```

**They have printed JSON on the success path all along.** What you hit was the *flag*: you passed `--json` because `sv verify-auto` established the idiom, got `error: unexpected argument '--json' found` with exit 2, and drew the reasonable conclusion that there was no structured output. The inference was sound; the premise was one layer off.

Worth saying because it changes what was actually blocking you — and because the fix is correspondingly smaller than "build three typed reports", which is what we would otherwise have gone and built.

## What changed

**1. `--json` is accepted on all three verbs.** Your existing command works.

**2. An error is now a JSON document too** — the substantive half. Before, success was JSON and failure was prose on stderr, so a consumer had to parse two formats and choose by exit code:

```console
$ mununu --quiet sv verify-liveness design.sv --request "nope == 1" --grant "x == 0" --json
{
  "verb": "sv-verify-liveness",
  "file": "design.sv",
  "error": "could not build the liveness monitor — an atom likely binds no signal in the design"
}
$ echo $?    # 1
```

The error document goes to **stdout**, beside the success document, so you read one stream. The prose still goes to stderr, because a human watching the same run should see it. `verb` and `file` are carried deliberately: a lane that runs many invocations into one log cannot attribute an error that says only `error`.

Both paths were validated in the `mununu-sva` image, not on the bare host — a host run of an SV path is presumed vacuous per our own rule, and in this case the host genuinely fails the lift (`syntax error, unexpected '@'`, i.e. sv2v without slang), which is a good reminder of why.

**Without `--json`, nothing changes.** Success still prints JSON, failure still prints prose. The flag is opt-in.

## ⚠️ What is still NOT guaranteed

**The success document is not schema-pinned.** `sv verify-auto`'s response is derived from Rust types via `schemars`, published under `docs/api-schemas/`, and guarded by a drift detector that fails CI on any wire-format change. These three summaries are still built ad-hoc with `serde_json::json!` — no types, no schema, no detector.

So the field set is **stable by convention, not by contract**. Your stated requirement was "the schema-pinned document, never the transcript", and this delivers the second half of that sentence but not yet the first. Pinning it is the remaining work on #541, and it shares a wire-format decision with [mununu#548](https://github.com/Mumunu-team/mununu/issues/548)'s O-2 (the per-property note join), so the two land together rather than deciding the report shape twice.

If you want to start consuming now, the fields above are what a run emits today; treat a new field as possible and an existing one as unlikely to move.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | none — no toolchain or dependency change | **No** |
| `mununu-sva` | none | **No** |
| `mununu-sva-pono` | none | **No** |
| `hw-verif` | none — not a mununu image | **No** |

## Not covered here

- **The schema.** See above; the remaining half of #541.
- **`sv verify` / the `btor2` verbs.** Unchanged. #541's note that `sv verify` returns `Btor2VerifyResponse` — a type shared with the pure-BTOR2 verbs and so missing SV-specific lift information — is a separate shape question, and it belongs with the schema decision.
- **API responses.** Both verbs already have routes (`/api/v1/sv/verify-recoverability`, `/api/v1/sv/verify-liveness`), so surface parity was already met; this change is CLI-side output only.

---

**Provenance.** Issue: [mununu#541](https://github.com/Mumunu-team/mununu/issues/541), filed from rosf. Validated in the `mununu-sva` image. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
