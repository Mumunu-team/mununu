# Consumer briefing — 2026-09 `--cutpoint` was an UNDER-approximation; verdicts under a cut change

> **Audience:** monono (reported it; runs `--cutpoint` in its formal lane and has withdrawn a verdict over it), ROSF, any consumer that passes `cutpoint` / `--cutpoint`, and anyone feeding hand-written BTOR2 or a non-Yosys frontend to the exact engine.
>
> **Related:** [mununu#498](https://github.com/Mumunu-team/mununu/issues/498).
>
> **⚠ TL;DR — this is a soundness fix, and it CHANGES VERDICTS.** `--cutpoint` documented an over-approximation and delivered the opposite: a freed net was frozen at 0, so the model had *fewer* behaviours than the design. A safety property that is concretely **false** could come back **`HOLDS`**. If you have acted on any verdict produced under `--cutpoint`, **re-run it.** Verdicts obtained without cut points are unaffected.

## What was wrong

`cutpoint` frees a net to a Yosys `$anyseq`. BTOR2 encodes that as a `state` line with **no `next`** — the format's free variable, unconstrained at every step. Two independent code paths together read it as something else:

| | | Effect |
|---|---|---|
| no `next` | the next-state substitution only fires when a `next` function exists | left as identity ⇒ **held** |
| no `init` | *"all-`⊥` (every bit 0) for a register with no `init` line"* | **pinned to zero** |

A `$anyseq` therefore became a **constant zero**. On monono's reduced FSM, `fits` was nailed to 0, `req && fits` was never true, and the FSM never left idle — so the cut model was a strict *under*-approximation of the design.

`$anyconst` was never affected: Yosys emits it as `next(k) = k`, so it has a `next` line and is a genuine held register.

## The verdicts this produced

monono's `mini_fetch` reduction, and the same flip on the real `sprite_fetch` block:

| | unabstracted | cut, **before** | cut, **after** |
|---|---|---|---|
| `EF (st_q == 2)` | HOLDS | **VIOLATED** ✗ | **HOLDS** ✓ |
| `EF (st_q == 1)` | HOLDS | **VIOLATED** ✗ | **HOLDS** ✓ |
| `AG (st_q == 0)` | VIOLATED | **HOLDS** ✗ | **VIOLATED** ✓ |
| `EF (st_q == 3)` *(never assigned — control)* | VIOLATED | VIOLATED | VIOLATED |
| `AG EF (st_q == 0)` | HOLDS | HOLDS | HOLDS |

Both directions moved. **`AG (st_q == 0)` returning `HOLDS` is the serious one** — a universal safety property passing on a model that cannot move, in precisely the direction the documentation called sound.

## The fix

A `state` with no `next` is now classified as a **free input** at the STS-IR seam (`BtorSts::states_with_next`), so it is free at every step and quantifiable. Every consumer of the seam inherits the correct semantics — this was never `cutpoint`-specific.

**Scope is wider than `--cutpoint`.** Any BTOR2 reaching the engines with a no-`next` state was affected: `$anyseq` from any source, hand-written BTOR2, other frontends. `--cutpoint` is simply the shipped path that reliably produces one.

## What did NOT change

- Runs **without** `--cutpoint`, on designs with no `$anyseq`: every state has a `next`, so the classification is identical and **no verdict moves**. The full suite (2512 lib tests, 57 doctests) passes unchanged.
- `$anyconst` — still a held register.
- No wire format, no route, no CLI flag, no sidecar schema.
- The `control-slice` ScopeCaveat note still fires and still says a general `VIOLATED` under a cut may be spurious. That remains true for **alternating** properties (`AG EF`, νμ); for a plain existential `EF` a `VIOLATED` under a genuine over-approximation is sound, and the cut is now genuinely one.

## Diagnostic change: `state register(s)` count

`model: N state register(s)` counted raw `state` lines. Now that a no-`next` state is not a register, that count would have gone **up** as more nets were freed — in the very diagnostic whose job is to show state being cut away (monono read `9 vs 8` as evidence). It now counts states carrying a `next`. **Designs without `$anyseq` see no change**; a cutpointed run reports fewer, correctly.

## For monono

1. **Re-run everything decided under `--cutpoint`.** Verdicts obtained without cut points are untouched.
2. **`sprite_fetch`'s withdrawal was correct**, and it should now be re-decided rather than left withdrawn: under the fixed cut, `ann_guarantee_1` returns `HOLDS` and the guarantee/witness pair agrees. Whether the budget bound decides is a separate question — re-measure rather than assume.
3. **Your regression is now shipped**, in both directions: `e2e_cutpoint_stays_an_over_approximation_no_verdict_flips` asserts that *no* property changes verdict when a cut is applied. It fails on the pre-fix code with exactly your reported flip.
4. `verify.sh` files that pin expected verdicts under a cut will need updating — and the pinning is what let you catch this, so it earned its keep.

**Docker rebuild:** yes. This is a binary change.

## For ROSF

If ROSF passes `cutpoint` through the API, the same re-run advice applies. If it never sets cut points and its designs have no `$anyseq`, this is a no-op.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod) | verdict semantics under `--cutpoint` | **Yes** |
| mununu `Dockerfile.dev` | binary bump | **Yes** |
| mununu `Dockerfile.sva` | binary bump; the e2e regression runs here | **Yes** |
| mununu `Dockerfile.extract`, `.extract-*` | no engine path | No |
| rosf `Dockerfile` / `.dev` / `.hw` | consumes verdicts; changes only if cut points are used | **Yes if it uses `cutpoint`**, else No |
| monono Docker | reported it; formal lane uses cut points | **Yes** |
| mununu-ui | no type change | No |

## Verification

```bash
# Unit — the seam classification and both verdict directions (no slang needed):
cargo test -p mununu-core --lib -- state_without_next anyseq_shaped anyconst_shaped

# End-to-end through the real slang lift, in the pinned image:
docker run --rm -v "$(pwd)":/work -v mununu-target:/ct -w /work -e CARGO_TARGET_DIR=/ct \
  mununu-sva bash -c 'export PATH=$HOME/.cargo/bin:/opt/oss-cad-suite/bin:$PATH; \
    cargo test -p mununu-core --lib e2e_cutpoint_stays -- --ignored'
```

Both were confirmed to **fail on the pre-fix code** — the e2e reproduces monono's flip verbatim — and pass after.

## Provenance

- Issue: [mununu#498](https://github.com/Mumunu-team/mununu/issues/498), from monono's `sprite_fetch` formal lane, with a 30-line reduction (`mini_fetch`) supplied by the reporter.
- Fix: `BtorSts::states_with_next` in `crates/mununu-core/src/adapter/sts_ir.rs`.
- Policy: [`../policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here (follow-ups)

- **`no init ⇒ pinned to zero` is the same class, via a different mechanism.** It makes an `$anyconst` a constant *zero* rather than an arbitrary constant. Not fixed here: pinning uninitialised registers to their reset value is legitimate and often intended for RTL, so changing it has its own blast radius and deserves its own decision. The regression test for `$anyconst` deliberately asserts the *classification* rather than a verdict, to avoid entangling the two.
- **The guarantee/witness disagreement flag**, which is what mununu#498 originally asked for. Still worth building — it is what would have surfaced this as a defect rather than leaving a careful author to catch it by hand — but this particular disagreement disappears with the fix.
- **`coverage-summary` tallies ⊥ as `skipped`**, so a harness under-reports its own undecided properties. Separate issue.
- **A pre-existing `#[ignore]`d test failure**, `e2e_cutpoint_frees_wide_counter_guard_so_exact_symbolic_fits`, which fails on a clean tree unrelated to this change — the `#[ignore]`d e2e set does not run in `make ci` and has drifted. Worth a scheduled run.
- **A trailing `// comment` after a `@mununu_guarantee` body** makes the annotation unparsable (`trailing characters after formula`). Minor, unrelated, found while reproducing.
