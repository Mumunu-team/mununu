# Consumer briefing — 2026-09 SV verify-auto ← fair-cycle l2s bridge (mununu#477 Option B, PR 2 — closes the ticket)

> **Audience:** ROSF (API consumer via `--profile industrial`), monono (direct CLI + API consumer, primary beneficiary), any orchestrator that runs `sv verify-auto` on SystemVerilog with `@mununu_assume` annotations.
>
> **Related:** [mununu#477](https://github.com/Mumunu-team/mununu/issues/477) — the ticket. Prior briefings on this track: [`2026-09-fairness-note-honesty.md`](2026-09-fairness-note-honesty.md) (Option A note honesty fix, mununu#487) and [`2026-09-fair-cycle-l2s.md`](2026-09-fair-cycle-l2s.md) (Option B PR 1 — the standalone `btor2 verify-liveness-under-fairness` verb, mununu#488). **This is Option B PR 2 and closes the ticket.**
>
> **TL;DR:** `// @mununu_assume GF <REG op VALUE>` in a SystemVerilog source is now **auto-applied** by `sv verify-auto` to any response-shape guarantee (`AG(a → AF b)`) in the same source. **No new verdict values; no new response fields; wire format unchanged.** The change is a runtime-dispatch upgrade: response-shape guarantees under fairness assumes now flow through PR 1's fair-cycle primitive instead of the plain response-liveness rescue, producing definite `holds` verdicts on cases that previously stayed `unknown`. Consumers observe the change as a new note kind (`fair-cycle-rescue`) accompanying properties the fair-cycle path decided.

## What changed

- **Typed fairness detection in the annotation scanner** — `crates/mununu-core/src/adapter/slang/verify_auto.rs`:
  - `AnnotationScan` gains `fairness_assumes: Vec<Atom>` alongside the existing `assumes: Vec<String>` bucket.
  - `// @mununu_assume GF <REG op VALUE>` bodies parse into typed `Atom` values (single register-comparison atoms). A body that fails to parse surfaces in `skipped` — never silently dropped.
  - Non-`GF` assumes (`// @mununu_assume clk_en = 1`) continue to flow through the untyped `assumes` bucket for the existing config-concretization path — unchanged.
- **Fair-cycle dispatch in `escalate_bottom`** — when the ⊥-escalation router hits a Liveness-class property AND `fairness_assumes` is non-empty AND the guarantee reduces to response shape, dispatch through `response_liveness_rescue_under_fairness` (mununu#488's primitive) instead of the plain `response_liveness_rescue`. Non-response guarantees fall through to the existing rescue path unchanged (soundness guard tested).
- **New `fair-cycle-rescue` verification-note kind** — accompanies any property the bridge decided; names the assumption(s) applied (`GF (grant_cpu == 1) && GF (fair_2 == 1)`).
- **`annotation-properties` note text upgraded** — the "recorded here" language becomes "auto-applied for response-shape guarantees"; the CTXDSL/btor2-verb pointer stays for cases the bridge doesn't cover.
- **Docs** — [`docs/verifying-rtl.md`](../verifying-rtl.md) gains a "fair-cycle bridge" section walking the `@mununu_assume GF` semantics; [`wiki/Property-Templates.md`](../../wiki/Property-Templates.md) soundness callout updated.

## What did NOT change

- **Wire format** — response shape unchanged; `SvVerifyAutoResponse` JSON schema unchanged (validated by the drift-detector — 17/17 pass with no regeneration needed).
- **`PropertyVerdict` values** — same canonical vocabulary.
- **CLI surface** — no new flags on `sv verify-auto`.
- **Existing rescue behaviour on properties without fairness assumes** — byte-for-byte unchanged.
- **PR 1's `btor2 verify-liveness-under-fairness` verb** — unchanged (the bridge calls the same library entry).

## For ROSF and monono

**What to update:** nothing forced. To use the bridge:

```systemverilog
// @mununu_assume GF grant_cpu == 1
// @mununu_guarantee nu Z. ((!(req == 1) || mu Y. ((ack == 1) || [] Y)) && [] Z)
module wb_mem_client(...);
    // ... your existing RTL ...
endmodule
```

```sh
# The guarantee decides under GF(grant_cpu == 1) automatically:
mununu sv verify-auto wb_mem_client.sv
# expected: verdict "holds" (previously "unknown" without the bridge)
```

Multiple `@mununu_assume GF <atom>` accumulate as a conjunction `(⋀ⱼ GF fairⱼ)` — the SV translator collects every `GF` assume in the source and applies them together to each response-shape guarantee.

**Report parsing impact:** additive-safe. A consumer that inspects `verification_notes[i].kind` may now see `"fair-cycle-rescue"` on properties the bridge decided. The verification-note SHAPE is unchanged.

**Soundness contract** — the same as PR 1:

- A `holds` verdict under `⋀ⱼ GF fairⱼ` means the response holds against every environment schedule satisfying each fairness constraint infinitely often.
- A `violated` verdict is a reachable lasso satisfying every constraint that still leaves the request forever ungranted.
- A useless fairness atom (env satisfies trivially) does NOT rescue a genuinely starving design — validated by mununu#488's soundness controls, which apply here transitively.

**Assumes not covered by the bridge:**

- State predicates as fairness atoms (`GF (fsm_state == IDLE)`) — the primitive accepts state atoms too, but the SV bridge currently only parses register-comparison shapes; use `btor2 verify-liveness-under-fairness` on the emitted BTOR2 for state-predicate fairness.
- Non-response guarantees (`AG p` safety, `AG EF good` recoverability) — fairness has well-defined semantics on them, but the fair-cycle primitive is response-specific.
- Coupled multi-guarantee fairness (`⋀ᵢ AG(aᵢ → AF bᵢ)` under one `⋀ⱼ GF fairⱼ`) — each guarantee's fair-cycle rescue runs independently; the coupled Streett shape is a further follow-up.
- Any of the above: use `mununu btor2 verify-liveness-under-fairness` on the emitted BTOR2 directly, or model in CTXDSL where the GR(1) game engine handles multi-pair coupled fairness.

**Docker rebuild:** binary bump only; no subprocess tool changes.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod) | binary picks up the SV verify-auto bridge; user-observable on any SV source with `@mununu_assume GF <atom>` | Yes if consumers pin a version tag |
| mununu `Dockerfile.dev` | binary bump + 3 new bridge tests + 4 new scanner tests | Only if the dev workflow requires the new binary |
| mununu `Dockerfile.sva` | binary bump; the ignored slang-gated e2e set is unaffected | No — no e2e coverage in this PR |
| mununu `Dockerfile.extract`, `.extract-*` | no impact | No |
| rosf `Dockerfile` / `Dockerfile.dev` / `.hw` | consumes mununu CLI/API; verdicts on SV sources with `@mununu_assume GF` may flip from `unknown` to `holds` | No — behaviour updates on next binary pull; a monitoring gate that expected `unknown` should be revalidated |
| monono Docker (if any) | primary beneficiary — the `wb_mem_client`-shape case becomes decidable via annotation | No — pull the new binary and add `// @mununu_assume GF grant_cpu == 1` to the module |
| mununu-ui deployment | no impact | No |

## Verification

- `cargo test -p mununu-core --lib --features api escalate_bottom_fair_cycle` — 3 bridge tests pass (baseline VIOLATED, positive rescue HOLDS, soundness guard against non-response misroute).
- `cargo test -p mununu-core --lib --features api h5_gr1_` — 8 scanner tests pass (4 new: typed fairness parse, multi-accumulation, malformed skip, non-fairness stays in string bucket).
- `cargo test -p mununu-core --lib --features api api_schema_drift_ -- --test-threads=1` — 17/17 schema drift tests pass (no regen needed; wire shape unchanged).
- End-to-end (post-merge): a SystemVerilog module with `// @mununu_assume GF grant_cpu == 1` + a response-shape `@mununu_guarantee` returns `holds` on `sv verify-auto` — no CLI flags needed.

## Provenance

- Fix commit: (pending merge — branch `feat/477-b-sv-verify-auto-fairness-bridge`).
- Ticket: [mununu#477](https://github.com/Mumunu-team/mununu/issues/477) — **closes on merge**.
- Prior briefings on this track: [`2026-09-fairness-note-honesty.md`](2026-09-fairness-note-honesty.md), [`2026-09-fair-cycle-l2s.md`](2026-09-fair-cycle-l2s.md).
- Design record: `.claude/plans/477-b-fair-cycle-l2s.md` — plan doc; this PR completes its Phase 4-6.
- Policy: [`../policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here (documented non-goals)

- **State-predicate fairness on the SV bridge.** The primitive supports state atoms; the SV scanner currently accepts register-comparison shapes. Trivial follow-up if a consumer asks.
- **Coupled multi-guarantee Streett shape.** Each guarantee's fair-cycle rescue runs independently in this PR.
- **General LTL assumptions.** The ticket explicitly narrows to `GF <signal>` conjunctions.
- **`sv verify-liveness-under-fairness` SV-wrapped verb.** Not shipped since the SV auto-routing bridge here supersedes the main use case; can be added additively if a consumer needs it directly.
