# Consumer briefing: an owned bit-level BMC on CaDiCaL, `btor2 verify --bmc-only --bmc-engine sat` (2026-11)

> Source of truth: [`sat_bmc::sat_bmc`](../../crates/mununu-core/src/adapter/btor2/sat_bmc.rs) — surface: CLI-only — an engine-evaluation switch of the CLI-only `--bmc-only`; the engine reaches the API when it joins the reach portfolio (Track N, N5.4)

**Audience:** anyone who runs `mununu btor2 verify --bmc-only`, or builds mununu from source.

## TL;DR

- **New flag.** `btor2 verify --bmc-only` takes `--bmc-engine native|sat`, with `native` as the default (unchanged).
  - `sat` runs Track N's first owned SAT engine, an in-process bit-level BMC. The design is bit-blasted to an and-inverter graph (AIG) and unrolled into CaDiCaL 1.9.5.
  - Both engines search every depth up to `--bmc-k`.
  - Under `sat`, `--timeout-ms` is the search's wall deadline.
- **Counterexamples only.** A counterexample is `violated`. No counterexample within the bound is `unknown`, because a bounded search proves nothing.
- **New JSON field: `bmc_stopped`.** `null` unless the `sat` search stopped short; then it says at which depth and why (the deadline, a cancellation).
- **Also in the JSON:** `engines` now names the engine, e.g. `bmc-only(k=80, sat-bmc)`, and `reachable_by` reads `sat-bmc` or `native-bmc`.
- **Refused:** designs with arrays. The command errors and names the array.
- **Build.** mununu now compiles CaDiCaL's C++ source through the `cadical` crate (MIT). That takes a C++ compiler, which every supported host already has, and no libclang.
- **No verdict changes.** The default path, the portfolio and the API are untouched.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Nothing on the default path. `--bmc-only`'s `engines` string now carries the engine name. | Only if a script matches `engines` exactly: accept the new form. |
| **monono** | Same. | Same. |
| **mununu-ui** | No UI surface. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the flag. | They ship the `mununu` binary. The build stage needs `g++`, which it already has. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Origin:** roadmap Track N, step N5.0 (the CNF emitter plus SAT core of the owned IC3). Item 6's C1 needs the same engine for the deep-counterexample class (`mul9`, #997).
- **Dependency choice, measured 2026-11-13:**
  - `rustsat-cadical` runs bindgen on every build, so it would need libclang on every host and image, and `mununu-sva` has none.
  - `cadical` needs only `cc`. It built and ran in `mununu-sva` with incremental assumptions, cores, a conflict limit and a terminate callback.
- **Tests:**
  - Every BTOR2 operator is checked bit for bit against the concrete evaluator at widths 1–8, or against an SMT-LIB reference where the concrete evaluator has none (division, remainder, overflow). A negative control confirms that breaking a gadget fails the tier.
  - `sat_bmc_equals_the_exact_engine_and_its_traces_replay`: on the random-BTOR2 tier, searched to depth 2^bits, the engine's answer equals the exact engine's both ways (96 designs on the gate's draw, 600 on a fresh one), and every counterexample replays on the concrete simulator.

## Not covered here

- **The engine in the reach portfolio** is N5.4.
- **The dynamic step for deep counterexamples** (item 6's C1) builds on this engine. It is not in this change.
- **The owned IC3** (N5.1–N5.3) builds on the same AIG and SAT core.
