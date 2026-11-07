# Dual-frontend SoC — RTL frontend unification, exercised

> **Industrial example** for [Document B — RTL frontend unification](../../../docs/archive/design/rtl-frontend-unification.md) §B.8. Exercises the seam-vs-core principle against the mununu binary on `main`: the open pieces of the SoC compose as ordinary mununu automata; the closed-IP DDR3 PHY is modelled as a 2-state chaotic stub with uncontrollable outputs, and the new `mununu contract sidecars` CLI produces the same JSON shape an auto-emitting adapter will produce once Document B's yosys-side integration ships.

## What this example is

A small SoC fragment with three components:

```
┌──────────────────────────────────────────┐
│ Host controller (open)                   │
└──────────────────────────────────────────┘
       │                       │
       ▼                       ▼
┌──────────────┐       ┌─────────────────────┐
│ UART (open)  │       │ DDR3 PHY (closed-IP │
│              │       │ black box)          │
└──────────────┘       └─────────────────────┘
```

The host controller drives both the UART peripheral and the DDR3 PHY. The UART is fully open and its state machine is verifiable end-to-end. The DDR3 PHY is `(* blackbox *)` in real RTL; in this CTXDSL model it appears as a 2-state automaton (`Idle` / `Busy`) with an *uncontrollable* `ddr_ready` transition — exactly the chaotic-stub default from [Document A §2](../../../docs/archive/design/black-box-modules.md#2-extracting-the-interface--unified-discovery-pipeline).

## What this example demonstrates

| Concept (Document B §) | How the example exercises it |
|---|---|
| §B.3 row "Black-box submodule handling" + §B.7.3 — adapter-side sidecar emission | `validate.sh` step 1 runs `mununu contract sidecars` against `blackbox_interfaces.json`, producing the same `<module>.interface.json` + `<module>.gap_report.json` files an auto-emitting adapter will produce once Document B's yosys integration ships. |
| §B.3 row "Controllability at top-level inputs" — shared rule | The DDR3 PHY's `ddr_ready` is `Output` direction → classified `Uncontrollable` from the host side via the rule shipped in Document A task A4 ([crates/mununu-core/src/controllability.rs](../../../crates/mununu-core/src/controllability.rs)). |
| Document A §A3 — gap-marker diagnostics | `validate.sh` step 5 runs `mununu contract gaps --strict-contracts` against the auto-emitted gap report; mununu emits one `WARN contract gap detected — chaotic stub default in effect` per gap and exits non-zero. |
| Document B §B.4 — two precision tiers (planned) | The SoC composition is verifiable at the *protocol* tier today (the safety + reachability properties over the host FSM hold). The same SoC will be verifiable at the *bit-level* tier once the yosys-side integration lands — `validate.sh` will then gain a step that runs the yosys adapter and cross-checks the auto-emitted sidecars are byte-identical with the hand-authored ones. |

## What this example does NOT claim

Per the [CLAUDE.md claims-integrity rules](../../../CLAUDE.md):

- It does **not** claim mununu found a bug in any commercial DDR3 PHY or any specific SoC.
- It does **not** demonstrate vendor `@mununu_guarantee` source-comment annotations (those land in M3, Document D's relocated task A6).
- It does **not** prove that any real SoC using a closed DDR3 PHY is correct. The proof is conditional on the chaotic-stub contract; a vendor-supplied latency-bound contract would tighten it.
- The custom-SV adapter does **not** yet auto-emit sidecars from its multi-module path. That is the custom-SV half of task B3, deferred to a follow-on commit. The yosys-side auto-emission *is* live in this commit (steps 5a–5c of `validate.sh`).

## How to run it

From the repo root:

```bash
./examples/industrial/dual_frontend_soc/validate.sh
```

The script builds the `mununu` binary, runs every command exercised in the demonstration, strips per-run noise (timestamps + ANSI escapes), and writes a byte-deterministic transcript to `transcript.txt`. Re-running `validate.sh` against the same commit produces an identical `transcript.txt` — verified via `diff`.

### Step-by-step

**Manual path** (hand-authored interface → sidecars):

1. **`mununu contract sidecars blackbox_interfaces.json --out-dir sidecars_generated`** — emits two JSON files (interface + gap report) per module described in the input. One DDR3 PHY → two sidecars.
2. **`cat sidecars_generated/DDR3_PHY_V2.interface.json`** — the auto-emitted interface description (8 ports, three Output / five Input).
3. **`cat sidecars_generated/DDR3_PHY_V2.gap_report.json`** — the auto-emitted phase-1 gap report (one `OutputSequencing` gap covering the three output ports).
4. **`mununu contract discover sidecars_generated/DDR3_PHY_V2.interface.json`** — runs phase-1 discovery against the auto-emitted interface. Same gap appears with a `WARN` diagnostic.
5. **`mununu contract gaps sidecars_generated/DDR3_PHY_V2.gap_report.json --strict-contracts`** — strict mode exits non-zero because the gap is unmet.

**Automatic path** (yosys adapter on `soc.sv` → sidecars):

5a. **`mununu context summarize soc.sv --adapter yosys`** — runs the yosys frontend. The driver detects `(* blackbox *)` on `ddr3_phy_v2`, parses the pre-flatten hierarchy snapshot (yosys `write_json`), and **auto-emits** `ddr3_phy_v2.interface.json` + `ddr3_phy_v2.gap_report.json` next to the source file. The `source_file` field points at the user's actual `.sv`, not the yosys tempdir.

5b–5c. The transcript shows the auto-emitted JSON content. Compare with the hand-authored version from step 1: the schemas match (same module name, ports, directions, gap kinds). The producer is different; the consumer (`mununu contract discover` / `mununu contract gaps`) cannot tell.

**Property verification on the hand-authored CTXDSL**:

6. **`mununu context eval soc.ctxdsl --formula soc_well_formed --automaton SoC`** — safety property over the composed SoC. Holds in 1/1 reachable composed states (under chaotic DDR).
7. **`mununu context eval soc.ctxdsl --formula burst_path_reachable --automaton HostController`** — host can reach the DDR burst path.
8. **`mununu context eval soc.ctxdsl --formula uart_send_reachable --automaton HostController`** — host can reach the UART send path.

The expected transcript is checked into `transcript.txt`.

## Files

| File | Purpose |
|---|---|
| `soc.ctxdsl` | Hand-authored composition of HostController + UART + DDR3 PHY chaotic stub. Three mu-calculus formulas. |
| `soc.sv` | SystemVerilog source for the same SoC. The `(* blackbox *)` attribute on `ddr3_phy_v2` is what the yosys auto-emission detects. |
| `blackbox_interfaces.json` | Black-box interface description for the manual path. Fed to `mununu contract sidecars`. |
| `sidecars_generated/` | Output of `mununu contract sidecars` (manual path). Regenerated by `validate.sh`. Gitignored. |
| `ddr3_phy_v2.{interface,gap_report}.json` | Output of the yosys auto-emission (steps 5a–5c). Regenerated by `validate.sh`. Gitignored. |
| `validate.sh` | Reproducible end-to-end runner. |
| `transcript.txt` | Byte-deterministic expected output. |

## Looking ahead

The yosys auto-emission is in place (steps 5a–5c). What's still deferred:

- **Custom-SV half of B3.** The custom-SV adapter does not yet auto-emit sidecars from its multi-module path. Today the user has to either run the yosys adapter, or hand-author the interface JSON. Custom-SV-side auto-emission is the next follow-on commit.
- **Full B1.** The yosys path detects black-box modules and emits sidecars, but the BTOR2 reader still uses its "all inputs uncontrollable" default for top-level inputs (which happens to match the controllability rule for *top-level* SV inputs, but is hard-coded rather than derived). Plumbing the captured port directions through is a separate refinement.
- **Side-by-side cross-check.** Once the custom-SV half of B3 lands, `validate.sh` will gain a step that runs both adapters on the same source and `diff`s the auto-emitted JSONs — byte-equality is the load-bearing verification of the dual-frontend principle.
