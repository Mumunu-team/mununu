# Consumer briefing — 2026-10 the reach portfolio's wall is now the first decider, not the slowest member

> **Audience:** monono, ROSF, and any consumer that times `btor2 verify`, `btor2 verify-liveness*`, `sv check-fsm` or `sv verify-auto` (its ⊥ re-plan), or that reads `decided_by` / `reachable_by` / `unreachable_by`.
>
> **Provenance:** category 4 (scheduling) of the engine-performance roadmap, item S1. Measured with the per-member instrument added in the same PR (`RUST_LOG=mununu_core::adapter::reach_portfolio=debug`). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## TL;DR — same verdicts, much earlier; `decided_by` may list fewer engines

Every reachability-portfolio call now ends when its first **trusted** definite verdict lands plus a 250 ms grace: the other members are cancelled (btormc, Pono, SPACER's isolated child and interpolation's cvc5 child are killed; native k-induction stops at its next depth). Before, the parallel driver waited for every member's own budget, and the driver behind the rescue verbs ran all six members **in series** whatever the first had decided.

| call (default budgets) | before | after | decided by |
|---|---|---|---|
| `btor2 verify`, 12-bit two-counter forward (host, no btormc/pono) | 10.13 s | 0.43 s | exact at 90 ms; SPACER's child ran to its 10 s timeout, native to 5 s |
| same, in `mununu-sva` (btormc + pono present) | 10.13 s | 0.37 s | exact at 73 ms; btormc 49 ms unknown; pono killed at 273 ms, SPACER 338, native 354, interp 358 |
| `btor2 verify-liveness`, i2c monitor (host / `mununu-sva`) | 0.30 s / 0.34 s | 0.36 s / 0.33 s | native at 55 ms (btormc at 44 ms in the image); the serial SPACER → btormc → Pono tail is gone, the grace remains |

Verdict values do not change: a member is cancelled only **after** a trusted definite verdict exists, and every member is individually sound.

## What changed

1. **One driver.** `decide_reach_portfolio` is now the parallel, cancelling driver at the default subprocess budget; `decide_reach_portfolio_parallel` is gone (its callers moved to the short name; `decide_reach_portfolio_parallel_with_timeout` is unchanged and still behind `btor2 verify --timeout-ms`). The sequential driver that backed `verify-liveness`, `verify-liveness-all`, `verify-liveness-under-fairness`, `check-fsm`, the recoverability vacuity probe and non-vacuity gate, and verify-auto's ⊥ re-plan no longer exists — those paths now overlap their members and stop at the first decider.
2. **Cancel-on-decided for every bounded member.** btormc, Pono and the isolated SPACER child go through one subprocess loop that kills the child 250 ms after the flag is observed; native k-induction checks the flag before each depth; interpolation's cvc5 query is killed the same way. The 250 ms grace is the owned-only driver's straggler grace applied uniformly, so members that co-complete within it keep their attribution and still feed the contradiction alarm.
3. **A lone SPACER `reachable` no longer counts as a decision.** `collect` already dropped it as uncorroborated; it now also does not cancel the members whose corroboration it needs (before, it silently cancelled the interpolation member).

## What to update, per consumer

### monono

- **Expect lower walls on the verbs above, and on `sv verify-auto` runs that rescue many ⊥ properties.** Any pinned timing budget (`--time-budget`, CI step timeouts) can stay; it will simply fire less often.
- **`decided_by` is now "who finished by the merge", not "every engine that would have agreed".** A property that used to list `["exact","native","spacer"]` may list `["exact"]` when the others would have landed more than 250 ms later. Do not key a gate on the *number* of deciders; the verdict is unchanged.
- **A contradiction that would have surfaced late is no longer surfaced.** The inter-engine `Contradiction` alarm (which maps to `unknown`) now covers only members that finish inside the grace. In the HWMCC study and on the corpus no such late disagreement was ever observed; if you keep an oracle lane for that purpose, the owned-only driver already made the same trade.
- **Sub-second calls can be slightly slower.** On a monitor every member decides in ~50 ms, the call now takes first-decision + 250 ms (0.30 → 0.36 s measured) because the grace is waited out. This is bounded and uniform; the multi-second tails it removes are not.

### ROSF

- No code change. The `mununu` lane's outcome vocabulary and report shape are unchanged; expect shorter lane times.

### Report-parsing impact

None. Field names, types, the JSON schemas and the outcome vocabulary are unchanged. `decided_by` contents (an engine-name list) can be shorter.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | wall-clock of the portfolio verbs; `decided_by` contents | **Yes** |
| `mununu-dev` | test image; carries the new unit tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; btormc/Pono members now cancelled on decision | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
# 1. The mechanism, no external tools needed:
cargo test -p mununu-core --lib -- a_cancelled_child_is_killed decide_bad_safety_cancel \
  adapter::reach_portfolio::tests

# 2. Per-member attribution on one of your designs (which member set the wall, who was cancelled):
RUST_LOG=mununu_core::adapter::reach_portfolio=debug MUNUNU_SELF_EXE=$(which mununu) \
  mununu --quiet btor2 verify design.btor2

# 3. Re-run a corpus and diff the `verdict` column: it must be identical. Diff `decided_by`
#    separately — shorter lists are this change; a different VALUE is not, and is worth reporting.
```

## Not covered here

- **The in-process SPACER fallback** (no `MUNUNU_SELF_EXE`, i.e. embeddings without the CLI binary) has no cancellation point; it still runs to its own timeout. The CLI and the API server always set `MUNUNU_SELF_EXE`.
- **The 250 ms grace is a constant** (`MEMBER_CANCEL_GRACE`), not a flag. If a consumer needs it tuned, that is a one-line change with a measurement to justify it.
- **A design nothing decides** is unchanged: every member runs to its own budget, as before (measured: the 16-bit forward two-counter on this branch, 23 s on the host and 10 s in `mununu-sva`, interpolation's and SPACER's caps being the poles — the #606 ordering fix makes the exact member decide it at 0.9 s, after which S1 applies; `ponylink-slaveTXlen-sat`, 18–22 s, nothing decides).
