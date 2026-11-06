# Consumer briefing — native BMC and k-induction decide designs with memories (2026-11)

> Source of truth: [`encode_for_bmc`](../../crates/mununu-core/src/adapter/btor2/native_bmc.rs) + [`encode_array_inits`](../../crates/mununu-core/src/adapter/sidecar/predicate_image/btor2_encode.rs) — surface: CLI+API — every route and verb that runs the reach portfolio (`btor2 verify`, `sv verify-auto`'s safety rescue, the liveness / recoverability rescues; `POST /api/v1/btor2/verify`, `POST /api/v1/sv/verify-auto`), and `btor2 verify --owned-only` / `--bmc-only`.

**Audience:** rosf, monono, and anyone who runs mununu on RTL with memories (register files, FIFOs, RAMs kept as BTOR2 arrays).

## TL;DR

- **Native BMC no longer refuses memory designs.** The in-process engine (native BMC + k-induction on z3) used to refuse any design with an array-sorted state (`native BMC: encode failed: … array …` in a member's `refused`). Since N5 it encodes such a design under QF_AUFBV:
  - `read` becomes `select`, `write` becomes `store`;
  - a memory's next state is an extensional array equality;
  - a memory's `init` is applied at frame 0: a bit-vector value fills every cell, an array expression is taken as is.
- **Verdicts can change from ⊥ to decided.** A safety row on a design with a memory in its cone, which no member decided before, can now be decided by `native`. Where btormc / Pono / rIC3 already decided it, `native` now also appears in `reachable_by` / `unreachable_by`. A `Contradiction` between `native` and another member would be a soundness alarm, as always.
- **Pure bit-vector designs are unchanged.** They are still encoded under QF_BV, byte for byte.
- **No JSON shape change.** `native` is an existing member name.

## Soundness

- **`Violated`** is a concrete run from an initial state, as before.
- **`Safe`** is a k-inductive proof. The induction step leaves each memory's contents free like any other state cell, so the proof covers every memory content.
- **A memory with an `init`** starts at it.
- **A memory without one** is free at frame 0, which is BTOR2's semantics.
- **An `init` the encoder cannot carry** (for example a memory initialised from an array-sorted input) makes native BMC refuse the design, with the reason in `refused`. It never leaves the memory free: that would make a counterexample spurious.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | rows on designs with memories may decide where they were ⊥, and `native` may appear in member lists | none; re-baseline any test that pins a ⊥ on such a design |
| **monono** | same as rosf, for `sv verify-auto` rows whose cone holds a memory | same |
| **mununu-ui** | none: no UI affordance for member lists | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes, if you verify designs with memories | the encoder is in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow verifies such designs | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes, if you run `sv verify-auto` / `btor2 verify` in the image | the binary the volume carries changes |
| `mununu/docker/Dockerfile.profile` | no | profiling image |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | yes, if rosf pins a mununu version and verifies designs with memories | picks up the change |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | yes, if monono pins a mununu version | picks up the change |

## Provenance

Track N, step N5 (the commercial-gap plan, 2026-11-03: "arrays in native BMC / k-induction").

Unit tests in `native_bmc.rs`:
- **A write then a read.** Violated at depth 1; refused before.
- **A zero-initialised memory that is never written.** k-induction proves it SAFE. With its `init` dropped it would be violated at depth 0, the init-soundness check.
- **The same memory without an `init`.** Violated at depth 0, the negative control.

The two refusal-attribution tests (mununu#734's `escalate_bottom_attributes_an_instant_inconclusive_to_the_refusing_members`, mununu#803's `the_owned_driver_records_the_native_members_refusal`) now trigger the refusal with an un-encodable memory `init`, not with a plain memory, which native BMC decides.

## Not covered here

- **The witness trace omits memory contents.** For a memory with an `init`, the inputs in the trace determine the run. For a memory without one, the counterexample depends on initial contents the trace does not list, so a replay of the trace alone may not reach `bad`.
- **The rest of N5 is not in this change:** the lift's `memory_nordff` on synchronous read ports, the census "memory in cone" column, and the btormc-reproduction gate on the edn rows.
