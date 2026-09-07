# Hardware Verification Patterns

> **Alpha Software** — Mununu is under active development. APIs, syntax, and behavior may change. We welcome feedback and bug reports via [GitHub Issues](https://github.com/Mumunu-team/mununu/issues).

This page collects hardware-focused verification patterns with abbreviated CTXDSL and key properties. Each pattern maps to a real protocol or hardware component and demonstrates how to model and verify it in Mununu.

---

## 1. Req/Ack Handshake

**Source**: `examples/hw/handshake.ctxdsl`

A 4-state FSM modeling a request/acknowledge handshake with latency. The environment controls the `req` signal; the controller manages ack assertion and timing.

**Protocol**: Environment asserts req (Idle -> WaitAck) -> controller counts latency cycles -> controller asserts ack (WaitAck -> Active) -> environment deasserts req (Active -> Done) -> controller deasserts ack (Done -> Idle).

**Key states**: `Idle` (initial), `WaitAck`, `Active`, `Done`

**Abbreviated CTXDSL**:

```
automaton Handshake {
    controllable {
        label ack_assert; label ack_deassert; label latency_tick;
    }
    states {
        state Idle initial; state WaitAck; state Active; state Done;
    }
    transitions {
        transition Idle -> WaitAck on label req_assert;
        transition WaitAck -> WaitAck on label latency_tick;
        transition WaitAck -> Active on label ack_assert;
        transition Active -> Done on label req_deassert;
        transition Done -> Idle on label ack_deassert;
    }
}
```

**Key properties**:

| Formula | Body | Checks |
|---------|------|--------|
| `safety_invariant` | `nu X. ([] X)` | All states are valid (safety) |
| `ack_reachable` | `mu X. (Active \|\| <> X)` | Active (ack asserted) is reachable |
| `cycle_completes` | `mu X. (Idle \|\| <> X)` | Full protocol cycle completes |
| `active_can_complete` | `(! Active) \|\| (mu Y. (Done \|\| <> Y))` | Active is not a dead end |

**Expected result**: All properties hold for all states. The controller can enforce the safety invariant.

---

## 2. Round-Robin Arbiter

**Source**: `examples/hw/arbiter.ctxdsl`

A 2-client arbiter with round-robin priority. When both clients request simultaneously, the arbiter grants the one not granted last. Grants are held until the client releases.

**Key states**: `Idle` (initial), `GrantA`, `GrantB`

**Abbreviated CTXDSL**:

```
automaton Arbiter {
    controllable { label grant_a; label grant_b; }
    states {
        state Idle initial; state GrantA; state GrantB;
    }
    transitions {
        transition Idle -> GrantA on label req_a;
        transition Idle -> GrantB on label req_b;
        transition Idle -> GrantA on label grant_a;
        transition Idle -> GrantB on label grant_b;
        transition GrantA -> GrantA on label req_a;
        transition GrantA -> Idle on label release_a;
        transition GrantB -> GrantB on label req_b;
        transition GrantB -> Idle on label release_b;
    }
}
```

**Key properties**:

| Formula | Body | Checks |
|---------|------|--------|
| `mutual_exclusion` | `(! GrantA) \|\| (! GrantB)` | Never both grants active (structural) |
| `grant_a_reachable` | `mu X. (GrantA \|\| <> X)` | Client A can be served |
| `grant_b_reachable` | `mu X. (GrantB \|\| <> X)` | Client B can be served |
| `grant_a_releases` | `(! GrantA) \|\| (mu Y. (Idle \|\| <> Y))` | Grant A eventually releases |

**Expected result**: Mutual exclusion holds structurally (GrantA and GrantB are distinct states). All reachability properties hold from all states.

---

## 3. Traffic Light Controller

**Source**: `examples/hw/traffic_light.ctxdsl`

A timer-driven 4-phase traffic light: Green -> Yellow -> Red -> RedWait -> (sensor) -> Green. The controller manages timer expiration; the environment controls the sensor trigger.

**Key states**: `Green` (initial), `Yellow`, `Red`, `RedWait`

**Abbreviated CTXDSL**:

```
automaton TrafficLight {
    controllable { label timer_expire; }
    states {
        state Green initial; state Yellow; state Red; state RedWait;
    }
    transitions {
        transition Green -> Green on label timer_tick;
        transition Green -> Yellow on label timer_expire;
        transition Yellow -> Yellow on label timer_tick;
        transition Yellow -> Red on label timer_expire;
        transition Red -> Red on label timer_tick;
        transition Red -> RedWait on label timer_expire;
        transition RedWait -> Green on label sensor_trigger;
        transition RedWait -> RedWait on label timer_tick;
    }
}
```

**Key properties**:

| Formula | Body | Checks |
|---------|------|--------|
| `full_cycle` | `mu X. (Green \|\| <> X)` | Complete cycle back to Green |
| `green_to_yellow` | `(! Green) \|\| < labels = { timer_expire } > Yellow` | Correct phase transition |
| `yellow_to_red` | `(! Yellow) \|\| < labels = { timer_expire } > Red` | Correct phase transition |

**Expected result**: All reachability properties hold. The labeled modality checks confirm correct phase ordering. Note: `full_cycle` depends on the environment triggering `sensor_trigger` at RedWait.

---

## 4. Ready/Valid Adapter (Skid Buffer)

**Source**: `examples/hw/rv_adapter.ctxdsl`

A 2-state FSM implementing a ready/valid backpressure adapter with a skid buffer. When the consumer deasserts ready while data is valid, the adapter captures data in a buffer (Empty -> Full). When ready reasserts, buffered data is sent (Full -> Empty).

**Key states**: `Empty` (initial), `Full`

**Abbreviated CTXDSL**:

```
automaton RvAdapter {
    controllable { label push; label drain; label passthrough; }
    states { state Empty initial; state Full; }
    transitions {
        transition Empty -> Empty on label passthrough;
        transition Empty -> Full on label push;
        transition Empty -> Empty on label idle;
        transition Full -> Empty on label drain;
        transition Full -> Full on label idle;
    }
}
```

**Key properties**:

| Formula | Body | Checks |
|---------|------|--------|
| `buffer_reachable` | `mu X. (Full \|\| <> X)` | Buffer can be used |
| `cycle_completes` | `mu X. (Empty \|\| <> X)` | Adapter returns to empty |
| `drain_possible` | `(! Full) \|\| (mu Y. (Empty \|\| <> Y))` | Buffer can always be drained |
| `push_possible` | `(! Empty) \|\| (mu Y. (Full \|\| <> Y))` | Buffer can always capture data |

**Expected result**: All properties hold. The adapter correctly implements the skid buffer protocol with no dead ends.

---

## 5. SPI Master (Single-Label Transitions)

**Source**: `tutorial/examples/02a_single_label.ctxdsl`

A serial peripheral interface master controller. Each transition carries exactly one signal event, modeling the sequential SPI protocol: assert chip-select, clock data bits, deassert chip-select.

**Key states**: `Idle` (initial), `Selected`, `Shifting`, `Complete`

**Abbreviated CTXDSL**:

```
automaton SpiMaster {
    controllable {
        label cs_assert; label cs_deassert;
        label sclk_tick; label mosi_write; label done;
    }
    states {
        state Idle initial; state Selected;
        state Shifting; state Complete;
    }
    transitions {
        transition Idle -> Selected on label cs_assert;
        transition Selected -> Shifting on label mosi_write;
        transition Shifting -> Shifting on label sclk_tick;
        transition Shifting -> Complete on label done;
        transition Complete -> Idle on label cs_deassert;
    }
}
```

**Key properties**:

| Formula | Body | Checks |
|---------|------|--------|
| `transfer_completes` | `mu X. (Complete \|\| <> X)` | Transfer can finish |
| `cs_leads_to_selected` | `(! Idle) \|\| < labels = { cs_assert } > Selected` | Labeled modality: cs_assert leads to Selected |
| `returns_to_idle` | `mu X. (Idle \|\| <> X)` | Cycle completes |

**Expected result**: All properties hold. The labeled modality check `< labels = { cs_assert } > Selected` verifies that the `cs_assert` label specifically leads to the `Selected` state from `Idle` -- a label-specific check not expressible in LTL.

---

## 6. AXI Bus Transaction (Multi-Label Transitions)

**Source**: `tutorial/examples/02b_multi_label.ctxdsl`

An AXI-like bus transaction controller where multiple signals fire simultaneously. Multi-label transitions model parallel signal assertion: address, data, and control signals assert together in one clock cycle.

**Key states**: `Idle` (initial), `WritePhase`, `ReadPhase`, `Acknowledged`

**Abbreviated CTXDSL**:

```
automaton BusController {
    controllable {
        label addr_valid; label data_valid;
        label ctrl_write; label ctrl_read; label idle;
    }
    states {
        state Idle initial; state WritePhase;
        state ReadPhase; state Acknowledged;
    }
    transitions {
        // Multi-label: three signals fire together
        transition Idle -> WritePhase
            on label addr_valid, label data_valid, label ctrl_write;
        transition Idle -> ReadPhase
            on label addr_valid, label ctrl_read;
        transition WritePhase -> Acknowledged on label ack;
        transition ReadPhase -> Acknowledged on label ack;
        transition Acknowledged -> Idle on label idle;
    }
}
```

**Key properties**:

| Formula | Body | Checks |
|---------|------|--------|
| `ack_reachable` | `mu X. (Acknowledged \|\| <> X)` | Transactions can complete |
| `write_reachable` | `mu X. (WritePhase \|\| <> X)` | Write transactions possible |
| `master_can_reach_ack` | `mu X. (Acknowledged \|\| [ (ctrl = controllable) ] X)` | Game-theoretic: master can force ack despite slave timing |

**Expected result**: Standard reachability holds for all states. The game-theoretic formula `master_can_reach_ack` checks whether the master can force reaching `Acknowledged` despite `ack` being uncontrollable (slave-driven). This fails for some states because the master cannot force the slave to acknowledge.

---

## 7. AMBA Bus Arbiter (GR(1))

**Source**: `examples/amba_arbiter_gr1.ctxdsl`

A 2-client AMBA AHB bus arbiter inspired by Bloem, Jobstmann, Piterman, Pnueli & Sa'ar, "Synthesis of Reactive(1) Designs" (JCSS, 2012). Clients issue requests; the arbiter grants exclusive access. The GR(1) contract requires mutual exclusion (safety) and no starvation (liveness under fairness).

**Key states**: Arbiter: `Free` (initial), `Grant0`, `Grant1`. Clients: `Idle`, `RequestingN`, `Holding`.

**Abbreviated CTXDSL**:

```
composition {
    asynchronous bus_system {
        members [Client0, Client1, Arbiter];
    }
}

mu_formulas {
    // Safety: mutual exclusion
    formula mutual_exclusion {
        over bus_system;
        body = (! Grant0) || (! Grant1);
    }

    // GR(1) liveness: GF(Requesting0) -> GF(Grant0)
    formula grant0_reachable {
        over bus_system;
        body = (! (nu X. (mu Y. (Requesting0 || <> Y)) && ([] X)))
            || (nu Z. (mu W. (Grant0 || <> W)) && ([] Z));
    }

    // GR(1) no starvation: GF(Grant0) -> GF(Free)
    formula no_starvation_0 {
        over bus_system;
        body = (! (nu X. (mu Y. (Grant0 || <> Y)) && ([] X)))
            || (nu Z. (mu W. (Free || <> W)) && ([] Z));
    }
}
```

**Expected result**: Mutual exclusion holds structurally. GR(1) liveness properties hold under the fairness assumption (if clients request infinitely often, they are granted infinitely often). The no-starvation formulas ensure the bus is freed infinitely often, preventing any client from monopolizing it.

A 4-client scaled version is available at `examples/amba_arbiter_gr1_synthesis.ctxdsl` with pairwise mutual exclusion checks for all 6 client pairs.

---

## 8. ALU with Accumulator (Kripke Mode)

**Source**: `examples/systemverilog/alu.sv`

An ALU with a 4-bit accumulator and externally driven command/operand inputs. Uses the **Kripke construction** — no explicit FSM enum for the accumulator or command. The command register is abstracted with a value-mapped enum: only the specific opcode constants used in the `case` statement are tracked, plus a catch-all `OTHER`.

**Annotations**:
```systemverilog
// @mununu mode kripke
// @mununu domain acc: bounded_counter 0..7
// @mununu domain cmd: enum {NOP=0, LOAD=1, ADD=2, SUB=3, CLR=4, OTHER}
// @mununu domain operand: bounded_counter 0..3
// @mununu input cmd, operand, start
```

**State space**: 8 (acc) x 6 (cmd) x 4 (operand) = 192 max, pruned by reachability.

**Key properties**:

| Formula | Body | Checks |
|---------|------|--------|
| `safety` | `nu X. ([] X)` | Well-formed automaton, no deadlock |

**Expected result**: Safety is realizable. The Kripke construction correctly models the ALU's accumulator arithmetic with bounded saturation.

**Why this matters**: Demonstrates that Mununu can verify register-level RTL beyond simple FSMs. The value-mapped enum abstraction (`cmd: enum {NOP=0, ...}`) collapses the 8-bit command space to 6 abstract values, making explicit-state verification tractable without losing precision on the relevant opcodes.

---

## 9. Synchronous FIFO Controller (Kripke Mode)

**Source**: `examples/systemverilog/fifo.sv`

A FIFO controller mixing an explicit `typedef enum` FSM (for the control interface) with a bounded counter (for the fill level). Uses the **Kripke construction** via `@mununu mode kripke` to model both dimensions together.

**Annotations**:
```systemverilog
// @mununu mode kripke
// @mununu domain fill: bounded_counter 0..4
// @mununu domain data_out_r: ignored
// @mununu input wr_en, rd_en
```

**State space**: 4 (state: IDLE/WRITING/READING/RDWR) x 5 (fill: 0..4) = 20 max.

**Key properties**:

| Formula | Body | Checks |
|---------|------|--------|
| `safety` | `nu X. ([] X)` | No deadlock, well-formed |

**Expected result**: Safety is realizable. The FIFO correctly handles simultaneous read/write, empty-to-full transitions, and maintains fill level invariants. Cone-of-influence automatically excludes `data_out_r` (8-bit data register) since it doesn't affect control flow properties.

**Why this matters**: Shows how `@mununu domain` annotations let you mix enum FSMs with register-level state (counters, flags) in a single model. The data path (`data_out_r`) is excluded from the state space without losing precision on the control properties.

---

## Pattern Summary

| Pattern | States | Controllable | Key Property Type | Source |
|---------|--------|-------------|-------------------|--------|
| Req/Ack Handshake | 4 | ack, latency | Safety + Reachability | `examples/hw/handshake.ctxdsl` |
| Round-Robin Arbiter | 3 | grants | Mutual Exclusion + Reachability | `examples/hw/arbiter.ctxdsl` |
| Traffic Light | 4 | timer_expire | Phase Ordering (labeled modality) | `examples/hw/traffic_light.ctxdsl` |
| Ready/Valid Adapter | 2 | push, drain, passthrough | Completeness (drain/push) | `examples/hw/rv_adapter.ctxdsl` |
| SPI Master | 4 | all (full control) | Labeled Modality | `tutorial/examples/02a_single_label.ctxdsl` |
| AXI Bus Transaction | 4 | master signals | Game-Theoretic Reachability | `tutorial/examples/02b_multi_label.ctxdsl` |
| AMBA Bus Arbiter | 3+3+3 (composed) | grants, idle | GR(1) No-Starvation | `examples/amba_arbiter_gr1.ctxdsl` |
| ALU with Accumulator | 192 max | acc | Kripke + value-mapped enum | `examples/systemverilog/alu.sv` |
| FIFO Controller | 20 | (control) | Kripke + bounded counter | `examples/systemverilog/fifo.sv` |
| FIFO Overflow Bug/Fix | 18 | (control) | Safety (overflow detection) | `examples/systemverilog/fifo_overflow_*.sv` |
| AXI-Lite Overlap Bug/Fix | 8 | (control) | Safety (overlapping transactions) | `examples/systemverilog/axilite_deadlock_*.sv` |
| CWE-1245 FSM Bug/Fix | 4 | (control) | Reachability (recovery from undefined) | `examples/systemverilog/cwe1245_fsm_*.sv` |

---

## 10. FIFO Overflow Bug (Sidecar Pipeline)

**Source**: `examples/systemverilog/fifo_overflow_bug.sv` + `.mununu.json`

FIFO controller missing `if (fill < DEPTH)` guard — fill exceeds capacity. Uses the `.mununu.json` sidecar pipeline with `fill` bounded to 0..5 so overflow to 5 is observable.

**Buggy:** unrealizable for `no_overflow` (fill_5 states are reachable).
**Fixed:** realizable (guard prevents overflow).

---

## 11. AXI-Lite Overlapping Transactions (Xilinx Bug)

**Source**: `examples/systemverilog/axilite_deadlock_bug.sv` + `.mununu.json`

Based on the Xilinx Vivado AXI-lite slave template bug documented by [ZipCPU](https://zipcpu.com/formal/2019/04/16/axi-mistakes.html). The slave accepts new write transactions while the previous response (`bvalid`) is still pending — corrupting state.

**Buggy:** unrealizable for `no_overlap` (8 states, overlapping states `bvalid_r_T_state_ADDR_WAIT` reachable).
**Fixed:** realizable (ready signals gated by `!bvalid_r`, only 5 reachable states).

---

## 12. CWE-1245: FSM with Undefined States (Security)

**Source**: `examples/systemverilog/cwe1245_fsm_bug.sv` + `.mununu.json`

Based on MITRE CWE-1245 with real CVEs (CVE-2024-21853, CVE-2024-24968). One-hot encoded FSM without `default` case — fault injection creates an absorbing undefined state that bypasses access control.

**Buggy:** 3/4 states satisfy `recoverable` — `state_UNDEF` cannot return to IDLE (stuck).
**Fixed:** 4/4 states satisfy `recoverable` — `default` branch forces recovery to IDLE.

## See Also

- [LTL Properties](LTL-Properties.md) -- temporal logic syntax for specifying properties
- [Controller Synthesis](Controller-Synthesis.md) -- synthesizing controllers from specifications
- [Counterstrategy](Counterstrategy.md) -- diagnosing unrealizable specifications
- [CLI Reference](CLI-Reference.md) -- running evaluations and synthesis from the command line
