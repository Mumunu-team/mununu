# BTOR2 Examples

BTOR2 (Niemetz–Preiner–Wolf, FMCAD 2018) is the de facto open-source word-level verification IR — emitted by Yosys's `write_btor` pass and consumed by Pono, AVR, BtorMC, and (now) mununu. The mununu BTOR2 adapter parses the file, bit-blasts state and input bit-vectors into explicit valuations, and turns each `bad` / `constraint` / `fair` / `justice` line into a μ-calculus property over the resulting CLTS.

These examples were added in **Phase 1 of the RTL roadmap** (`adapter::yosys` + `adapter::btor2`). They demonstrate the SV→Yosys→BTOR2→CLTS pipeline end-to-end.

## How the BTOR2 reader treats the design

The BTOR2 reader applies four transformations on top of Yosys's elaboration to produce a CLTS that matches mununu's natural model:

1. **Per-signal labels.** Each transition carries one `<signal>=<value>` label per non-clock input, e.g. `transition s0 -> s253 on label rst_0;`. Properties refer to individual signals (`[(rst_0)] φ`) rather than enumerating compound `env_NNNN` strings. The CLTS data structure already supports multi-label transitions; the reader pushes one label per signal-bit into `IRTransition.labels`.
2. **Implicit clock.** Each CLTS transition represents one clock edge; `clk` does not appear in the alphabet. The reader detects clock-shaped input names (`clk`, `clock`, `ck`, `clk_i`, `i_clk`, `iclk`, `clki`) and elides them from enumeration. Multi-clock and negedge designs are out of scope for Phase 1 — the reader errors explicitly.
3. **Synchronous Yosys script.** The Yosys driver uses `async2sync` (not `clk2fflogic`) before `chformal -lower`, preserving the synchronous structure. `clk2fflogic` would introduce a `value + shadow + previous-clk` triple-state-cell encoding per FF group; `async2sync` produces one state cell per FF, matching mununu's "transition = one clock edge" semantics natively.
4. **Enumerated state names + valuations side-channel.** State names are `s0, s1, …, sN-1`. Per-state register valuations are carried via `StateSpec.valuations` (the same mechanism the SV adapter uses) so user-written formulas like `state == 0` resolve via the on-demand expression evaluator. Synthetic `chformal -lower` property-tracking latches are filtered out of valuations — only user-named state cells appear.

## The example corpus

Each example demonstrates a different BTOR2 line-type and SV idiom. All four elaborate via the canonical `sv-yosys` driver script and produce models well under the `MAX_STATE_BITS = 20` cap.

### `safety_demo.sv` — counter with violatable assertion

A 2-bit free-running counter; `assert (cnt != 2'b11)` is violated by design every fourth cycle when `rst` is low. Emits 1 BTOR2 `bad` line. Expected verdict: `safety_bad_0` does **not** hold (bad state is reachable).

### `traffic_light.sv` — 3-state FSM, three safety assertions

A `RED → GREEN → YELLOW → RED` FSM with `typedef enum`, `always_ff`, `always @* case`. Three immediate `assert` statements ensure mutual exclusion of the three lights. 256 states, 3 BTOR2 `bad` lines. Expected verdict: all three `safety_bad_*` hold.

### `fair_arbiter.sv` — round-robin arbiter, mutual-exclusion safety

A 2-client round-robin arbiter with a 1-bit priority pointer. The user-named state register is the priority pointer; the design also has 2-bit `req` input and 2-bit `gnt` output. One `assert (!(gnt[0] && gnt[1]))` for mutual exclusion. 32 states. Expected verdict: `safety_bad_0` holds. (Per-client liveness — `each requester eventually granted` — would require SVA `s_eventually`, which Yosys 0.59's built-in parser does not accept; see "Yosys SVA support" below.)

### `handshake_protocol.sv` — AMBA-style valid/ready handshake (H1 only)

A small FSM that asserts `valid` on `request`, holds it until `ready`, then idles. Phase 5A's H1 (handshake stability) is encoded with a shadow register — `(valid && !ready)` last cycle implies `valid` this cycle. 32 states. Phase 5A's S1 (`$stable(payload)`) requires a second shadow register and adds enough state to push the design past the cap; it ships when compositional decomposition (Phase 3) lands or when sv2v preprocessing is wired in.

### `bounded_counter_with_assume.sv` — environment assume + safety

A 3-bit saturating counter with an `assume`-based environment constraint ("rst is not held two cycles in a row") and a safety assertion ("after a reset cycle, cnt is 0 or 1"). Demonstrates BTOR2's `constraint` line: pairs of `(state, input)` violating the assume are filtered out before evaluation. 256 states.

## Yosys SVA support — what the built-in parser actually accepts

**Scope: the yosys `read_verilog` lift used by this corpus** (`btor2 from-sv`, plain `sv verify`). Temporal SVA is not lost to mununu — `sv verify-auto` elaborates the design with slang and translates `assert property` itself; its fragment is listed in [`docs/verifying-rtl.md` §3](../../docs/verifying-rtl.md#3-coverage-is-a-fragment-and-verdicts-are-honest). What follows is what the **yosys** frontend accepts. Yosys 0.59's `read_verilog -formal -sv` is a synthesis frontend, not a full SystemVerilog assertion frontend. It accepts:

- **Immediate assertions** inside `always @(posedge clk)`: `assert (boolean_expr);`
- **Immediate assumes / covers**: `assume (...)`, `cover (...)`

It does **not** accept:

- Concurrent assertions: `assert property (@(posedge clk) ...)`
- Temporal SVA operators: `|->`, `|=>`, `##N`, `s_eventually`, `$stable`, `$rose`, `$past`, `nexttime`, etc.
- `default clocking` blocks
- PSL / FL syntax

The example corpus uses **shadow-register patterns** to encode property semantics that need temporal SVA — a pre-cycle latch tracks the antecedent, an immediate Boolean assertion tests the consequent. This is the canonical Yosys-flow workaround documented in YosysHQ's tutorials and mirrors how every working OSS SVA example is actually written.

Temporal SVA is verified by `sv verify-auto` (slang elaboration + mununu's own translator in `crates/mununu-core/src/adapter/slang/translate.rs`), independent of the yosys frontend; its supported fragment and the rejected forms are listed in [`docs/verifying-rtl.md` §3](../../docs/verifying-rtl.md#3-coverage-is-a-fragment-and-verdicts-are-honest).

## CLI

```bash
# Auto-detected by extension (uses Yosys for .sv, BTOR2 reader for .btor):
mununu context summarize examples/btor2/safety_demo.btor
mununu context eval examples/btor2/safety_demo.sv \
    --adapter sv-yosys --formula safety_bad_0 --automaton Circuit

# Explicit BTOR2 adapter on a hand-edited .btor:
mununu context eval examples/btor2/safety_demo.btor \
    --adapter btor2 --formula safety_bad_0 --automaton Circuit
```

Both forms yield the same model and verdict. The `sv-yosys` form requires a stock Yosys ≥ 0.40 on `$PATH` (or `MUNUNU_YOSYS_PATH` set); see [`adapter::yosys::translate_sv`](../../crates/mununu-core/src/adapter/yosys/mod.rs) for the driver.

## Regenerating `.btor` files from source

Inside this directory:

```bash
for f in safety_demo bounded_counter_with_assume fair_arbiter handshake_protocol traffic_light; do
  yosys -q -p "read_verilog -formal -sv $f.sv; hierarchy -auto-top; \
                proc; flatten; async2sync; chformal -lower; \
                dffunmap; setundef -zero; write_btor $f.btor"
done
```

The pipeline mirrors [`adapter::yosys::build_script`](../../crates/mununu-core/src/adapter/yosys/mod.rs). `async2sync` (rather than `clk2fflogic`) is what keeps the elaboration synchronous and lets the BTOR2 reader's "transition = one clock edge" semantics fit BTOR2 1:1 — see "How the BTOR2 reader treats the design" above.

## API

```bash
SV=$(cat examples/btor2/safety_demo.sv)
CTXDSL=$(curl -s -X POST http://127.0.0.1:8080/api/v1/context/import \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg c "$SV" '{format:"sv-yosys", content:$c}')" \
  | jq -r '.ctxdsl')
curl -s -X POST http://127.0.0.1:8080/api/v1/context/verify \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg c "$CTXDSL" \
        '{context:{name:"safety_demo", content:$c},
          formula:"safety_bad_0", automaton:"Circuit"}')"
```

Replacing `"sv-yosys"` with `"btor2"` and `$SV` with the contents of `safety_demo.btor` is equivalent.

## UI

`.btor` and `.btor2` are in `ADAPTER_EXTENSIONS` ([mununu-ui/src/api/endpoints.ts](../../../mununu-ui/src/api/endpoints.ts)) — the editor auto-routes a BTOR2 file through `/import`. For SV files, the toolbar exposes a small **`SV: [hand | yosys]`** dropdown next to the file picker — set it to `yosys` and open any `.sv` here to exercise the Yosys-driven path. Hand mode keeps the original FSM-class adapter for backward compatibility.

## Soundness notes (per CLAUDE.md rules)

- **Bit-blasting is exact** for the operators marked `is_blastable()` in [`adapter::btor2::ast::Op`](../../crates/mununu-core/src/adapter/btor2/ast.rs). No approximation.
- **State-space rejection over silent truncation:** designs whose total state-bit width exceeds [`MAX_STATE_BITS = 20`](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs) error out with `StateSpaceOverflow` — the documented escape hatch is compose-and-decompose (Phase 3) before BTOR2 hand-off to an external symbolic engine.
- **Implicit clock is sound for posedge-only single-clock designs.** Multi-clock and negedge are explicitly rejected at read time. The "one CLTS transition = one posedge" mapping is exact under that scope.
- **`async2sync` preserves synchronous structure** for both register cells and `chformal`-lowered assertions. The user-named state cells get one BTOR2 `state` line each (no shadow / previous-clk auxiliaries), and assertion-tracking latches that `chformal -lower` introduces are real synthetic state — they encode the temporal property "the assertion has fired in some prior cycle" — not edge detection.
- **`setundef -zero`** in the Yosys script makes X / undef bits deterministic (bit-blaster does not model X-prop). For X-aware verification, route through a commercial flow; that is out of mununu's roadmap scope.
- **Verific check** at runtime: the driver refuses to use a yosys binary built with the commercial Verific frontend (license-incompatible). See [`adapter::yosys::verify_no_verific`](../../crates/mununu-core/src/adapter/yosys/mod.rs).
- **`chformal -lower` property-tracking latches** are real CLTS state — they encode "the assertion has fired in some prior cycle." They appear in state names as anonymous `st<idx>_n<nid>` symbols and are filtered out of `StateSpec.valuations` so user-written formulas don't accidentally reference them.

## Optional sv2v preprocessing (modern SV dialect)

Yosys's built-in parser rejects the SV2009/2012 module-header `import pkg::*;` syntax used by Caliptra-RTL, OpenTitan, ibex, cv32e40p, and similar open-source RTL. The mununu sv-yosys driver can optionally run [zachjs/sv2v](https://github.com/zachjs/sv2v) as a preprocessing pass.

**Opt in** via either `MUNUNU_USE_SV2V=1` (env var) or `YosysOptions.use_sv2v = true` (programmatic). Requires `sv2v` (≥ 0.0.10 recommended) on `$PATH` or in `MUNUNU_SV2V_PATH`. See [`adapter::yosys::run_sv2v`](../../crates/mununu-core/src/adapter/yosys/mod.rs).

**What it does.** Before the Yosys subprocess, the driver invokes `sv2v -I<source-parent-dir> <all-input-sv>`, captures stdout into a temp `preprocessed.sv`, and feeds that single Verilog-2005 file to Yosys. Cross-file package resolution uses sv2v's documented multi-file mode — pass related `.sv` files together via `YosysOptions::additional_sources` so sv2v resolves them in one pass. `\`include` directives are searched relative to the source's parent directory.
