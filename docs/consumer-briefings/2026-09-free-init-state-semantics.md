# Consumer briefing — 2026-09 a BTOR2 `state` with no `init` is now FREE, per the format

> **Audience:** anyone passing a BTOR2 file to `mununu btor2 verify*` / `cegar` / `game` — hand-written, third-party (HWMCC, btor2tools), or emitted by a non-mununu frontend. **`sv verify-auto` users are unaffected**; see below.
>
> **Related:** [mununu#498](https://github.com/Mumunu-team/mununu/issues/498) (the sibling finding; the parent fix shipped in #502).
>
> **TL;DR:** the exact engine used to pin an un-`init`ed register to **zero**. The BTOR2 format leaves it **unconstrained**, which is what `btormc` and Pono implement. mununu now matches the format. Verdicts on the direct-BTOR2 path can change — **only ever `Holds` → `Violated`** — and the engine now *decides* cases where it previously had to abstain.

## What was wrong

`initial_state_bdd` conjoined `state_bit ⟺ value_bit` for **every** state, using all-zero for a state with no `init` line. The modelled initial set was therefore a strict **subset** of the design's, which is an **under-approximation**:

- **`Violated` was sound** — the pinned state is a genuine initial state, so a counterexample from it is real.
- **`Holds` was not** — φ holding at *one* initial state says nothing about the others.

The consequence was a differential disagreement with the rest of the portfolio. On btor2tools' `noninitstate.btor2` — a case that exists precisely to test this — `state0`/`state1` have no `init` and a `constraint` forces them *different* in the first step. Pinning both to 0 made them *equal*, contradicting the constraint, so the exact engine could not see the real initial states. `exact_bad_reachable` handled that by **abstaining**, leaving btormc / Pono / SPACER to carry the portfolio.

## What changed

A state with an `init` line is pinned to that value; a state with **no `init` line is left free**. The modelled initial set now *equals* the design's, so **both verdict directions are sound** and the abstention is no longer needed — it has been removed.

```
examples/btor2/btor2tools_suite/noninitstate.btor2

  before   reachable_by: [native, spacer, btormc, pono]        <- `exact` abstained
  after    reachable_by: [exact, native, spacer, btormc, pono] <- decides, and agrees
```

## Who is affected

| Path | Effect |
|---|---|
| **`sv verify-auto`, reset-gated (the default)** | **None.** `reset_init::inject_reset_init` injects an `init` line for *every* state cell before the model is built, so no free-init state exists |
| `sv verify-auto --no-gate-reset` | Behaviour changes — and arguably corrects; that flag is documented as *"the design chooses its own power-up"*, which is now what it models |
| `btor2 verify*` / `cegar` / `game` on files with free-init state | Verdicts may change, and previously-abstained cases now decide |
| Fully-initialised BTOR2 (every state has `init`) | **None** — identical model |

**Direction of change:** `Holds` gets harder (must hold from *every* initial state), `Violated` gets easier (one bad initial state suffices). Migrations only ever go `Holds → Violated`. A `Holds` that survives is now a stronger claim than the one it replaces.

## Measured blast radius

| | |
|---|---|
| Repo `.btor2` corpus | 7 fully-initialised (unaffected), 2 partially-initialised — both btor2tools cases where free-init is the *correct* semantics |
| Test suite | **1** test changed behaviour: the guard's own test, which asserted the abstention. Rewritten to assert the engine now decides. 2513 pass |
| Doctests, clippy, workspace | unchanged, clean |

## The residual risk, stated plainly

`inject_reset_init` is **all-or-nothing**: it no-ops entirely if the design already carries *any* `init` line. So an RTL design with *partial* init — some registers initialised by an `initial` block or an `(* init *)` attribute, the rest not — gets no injection, and under this change its uninitialised registers go free while reset is pinned inactive. That can produce a **spurious `VIOLATED`**.

Yosys `async2sync` emits no `init` at all (checked on three designs), so this needs hand-written init to arise. It is uncommon in RTL, not impossible. **If you see a new `VIOLATED` on an SV design, check whether its BTOR2 has partial init** — that is the shape to suspect, and it is worth reporting.

## For monono

Your formal lane runs `sv verify-auto` reset-gated, so this should be a no-op for you. It is worth one confirming run: if any verdict moves, the partial-init shape above is the thing to check, and I would want to hear about it.

The `--cutpoint` re-run advice from the #502 briefing still stands and is unrelated to this change.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod) | verdict semantics on the direct-BTOR2 path | **Yes** |
| mununu `Dockerfile.dev` | binary bump | **Yes** |
| mununu `Dockerfile.sva` | binary bump | **Yes** |
| mununu `Dockerfile.extract`, `.extract-*` | no engine path | No |
| rosf | changes only if it feeds BTOR2 with free-init state directly | **No** for the `sv` surface |
| monono Docker | reset-gated SV path — expected no-op | Recommended, not forced |
| mununu-ui | no type change | No |

## Verification

```bash
cargo test -p mununu-core --lib -- exact_decides_btor2tools_noninitstate \
                                   exact_bad_reachable_decides_free_init
```

The first runs against the real btor2tools file and asserts agreement with `btormc`'s `sat`; the second pins the mechanism on a minimal fixture. Both fail on the pre-fix code.

## Provenance

- Found while fixing [mununu#498](https://github.com/Mumunu-team/mununu/issues/498) — a test asserting `$anyconst` reachability failed, and the honest reading was that the test was right and the code was not.
- Fix: `BddBitBlaster::initial_state_bdd` in `crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs`.
- Policy: [`../policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here (follow-ups)

- **`--no-gate-reset` and CWE-1245.** That flag is documented as letting the design choose its own power-up "including the undefined-encoding scenarios CWE-1245 detection relies on". Under the old zero-pin every register powered up at 0, which may be a *legal* encoding — so that detection may have been blind to the scenario it exists for. This change plausibly fixes it. **Unverified**; worth one experiment.
- **`state_cell_init_values`** (the cube path) still defaults to 0 for a state with no `init`. It feeds a different engine and was not touched here; whether it has the same asymmetry is a separate question.
- **[mununu#504](https://github.com/Mumunu-team/mununu/issues/504)** — the exact engine can stack-overflow rather than abstain on a large design. Unrelated, found in the same sweep.
