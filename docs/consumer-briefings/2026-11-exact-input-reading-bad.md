# Consumer briefing — the exact engine decides an input-reading `bad` it used to miss (2026-11)

> Source of truth: [`exact_bad_reachable`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) + [`ExactModel::target_state_set`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) — surface: CLI+API — the `btor2 verify` safety-portfolio verdict path (RTL / BTOR2; no UI affordance).

**Audience:** rosf, monono and anyone who reads a `btor2 verify` / `sv verify-auto` safety verdict or the `/api/v1/btor2/verify` response.

## TL;DR

- **A soundness bugfix, verdict-value change on one narrow shape.** The exact full-state ROBDD engine (`exact_bad_reachable`) reported a reachable `bad` as **`unreachable`** when the `bad` node reads a primary input *and* the counterexample needs a **different input value at the `bad` cycle than at the transition cycle**. It now decides **`reachable`**, agreeing with native BMC, SPACER, interpolation and btormc (mununu#852).
- **No shipped verdict was ever wrong.** The portfolio saw the disagreement, flagged `"contradiction": true` and downgraded the merged `outcome` to `"unknown"`. After the fix, for this shape the engines agree and the merged `outcome` is `"reachable"` with a real counterexample — a `contradiction`/`unknown` becomes a definite `reachable`.
- **The common case is unchanged.** A `bad` that does not read an input, or whose counterexample uses the same input throughout, decides exactly as before. The fix projects the `bad` target's inputs to a pure state set before the `EF` fixpoint (`∃i.(bad ∧ constraint)`), which is identity when `bad` has no input bits.
- **No JSON shape change, no flag or route change.** `btor2 verify` / `btor2 verify --json` and `/api/v1/btor2/verify` keep their schema; only the `outcome` / `contradiction` / `reachable_by` *values* move for the affected shape.
- **The differential tier that caught it is now backed by a deterministic seam test** (`exact_bad_reachable_input_read_by_bad_is_independent_of_the_transition_input`); the random tier stays as the detector.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | a BTOR2/RTL safety property whose `bad` cone reaches a primary input may flip from a merged `unknown` (`contradiction`) to a definite `violated`/`reachable` with a counterexample; a property that was already definite does not move | none required; if you cached `unknown` verdicts on such properties, re-run them to pick up the now-definite answer |
| **monono** | same as rosf for any BTOR2 it verifies through the portfolio; CTXDSL / cube path unaffected | none; re-run cached `unknown` safety rows if any read an input in the `bad` |
| **mununu-ui** | none — the exact engine's BTOR2 safety verdict is not a CTXDSL capability and has no UI affordance | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag and verify BTOR2/RTL safety | engine verdict fix |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow needs the fixed binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes if you run `sv verify-auto` / `btor2 verify` for safety in the image | the fix lives in the binary the volume carries |
| `mununu/docker/Dockerfile.profile` | no | profiling image, verdict-independent |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | yes if rosf pins a mununu version and verifies safety | picks up the fixed engine |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | yes if monono pins a mununu version and verifies BTOR2 safety | picks up the fixed engine |

## Provenance

Found by the in-repo N0 random-BTOR2 differential tier (`crates/mununu-core/tests/engine/btor2_differential.rs`), proptest-shrunk to an 18-line design, root-caused to the `EF(bad)` pre-image tying the `bad`'s input to the transition input, and fixed by projecting the target's inputs to a state set first. Issue mununu#852; reproducer `exact_bad_reachable_input_read_by_bad_is_independent_of_the_transition_input` (fails on the parent commit, passes on the fix).

## Not covered here

The sibling `ExactModel::ef_region` (used by `verify-recoverability` and the env-strategy path) shares the `EF` fixpoint shape, but its target comes from `predicate_bdd`, whose `signal_bits` resolves only registers and named signals and whose input-reading atoms are refused or lifted into the state (`input_state_enabled`, mununu#656) rather than left free — so the untied-input case cannot arise there. `exact_bad_reachable` is unique in running the fixpoint on a raw `resolve`d `bad` that keeps input bits free, which is why it alone needed the projection. No change to the recoverability path.
