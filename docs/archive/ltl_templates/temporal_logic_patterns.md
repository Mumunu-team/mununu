> Status: historical — written in 2026-05 and deleted in `ce1da0c6` (PR #57, 2026-05-16); restored here on 2026-11-07 from `ce1da0c6^:docs/ltl_templates/temporal_logic_patterns.md` because module docs, READMEs and tests still cite it (roadmap D2). Not maintained since: names, paths and claims below describe the code as it was then. The live design notes are under `docs/design/`.

# Temporal Logic Patterns for Synchronous and Asynchronous Systems

This document catalogs common LTL (Linear Temporal Logic) and CTL (Computation Tree Logic) patterns used in formal verification of reactive systems, with practical use cases for both synchronous and asynchronous system models.

---

## Table of Contents

1. [Safety Properties](#safety-properties)
2. [Liveness Properties](#liveness-properties)
3. [Reactiveness Properties](#reactiveness-properties)
4. [GR(1) Patterns](#gr1-patterns)
5. [Counter Strategies for GR(1)](#counter-strategies-for-gr1)

---

## Safety Properties

Safety properties assert that "something bad never happens" - they are violated by finite execution traces.

### Pattern: Invariant (Always)

**LTL:** `G(φ)` - Globally, φ always holds  
**CTL:** `AG(φ)` - On all paths, globally φ holds

#### Use Case 1: Synchronous System - Mutual Exclusion

**Context:** Two processes sharing a critical section in a synchronous round-based system.

**Property:** No two processes can be in the critical section simultaneously.

**LTL Formula:**
```
G(!(in_critical_section_1 && in_critical_section_2))
```

**CTL Formula:**
```
AG(!(in_critical_section_1 && in_critical_section_2))
```

**Description:** In each synchronous round, at most one process can hold the critical section lock. This ensures mutual exclusion across all execution paths.

#### Use Case 2: Asynchronous System - Bounded Buffer

**Context:** Producer-consumer system with a bounded buffer of size N.

**Property:** The buffer never exceeds its capacity.

**LTL Formula:**
```
G(buffer_count <= N)
```

**CTL Formula:**
```
AG(buffer_count <= N)
```

**Description:** Regardless of the interleaving of producer and consumer actions, the buffer occupancy never exceeds N. This prevents buffer overflow in asynchronous message passing.

---

### Pattern: Response (Always Eventually)

**LTL:** `G(φ → F(ψ))` - If φ occurs, then eventually ψ will occur  
**CTL:** `AG(φ → AF(ψ))` - On all paths, if φ holds, then eventually ψ will hold on all paths

#### Use Case 1: Synchronous System - Request-Response Protocol

**Context:** Client-server interaction in a synchronous request-response protocol.

**Property:** Every request is eventually followed by a response.

**LTL Formula:**
```
G(request → F(response))
```

**CTL Formula:**
```
AG(request → AF(response))
```

**Description:** In a synchronous system where requests and responses occur in the same round or subsequent rounds, every request must eventually receive a response.

#### Use Case 2: Asynchronous System - Message Delivery

**Context:** Asynchronous message passing system with unreliable channels.

**Property:** If a message is sent, it will eventually be delivered (assuming fair scheduling).

**LTL Formula:**
```
G(send_message → F(message_delivered))
```

**CTL Formula:**
```
AG(send_message → AF(message_delivered))
```

**Description:** In an asynchronous system, messages may be delayed but must eventually be delivered under fair scheduling assumptions.

---

## Liveness Properties

Liveness properties assert that "something good eventually happens" - they cannot be violated by finite traces.

### Pattern: Eventually (Reachability)

**LTL:** `F(φ)` - Eventually φ will hold  
**CTL:** `EF(φ)` - There exists a path where eventually φ holds

#### Use Case 1: Synchronous System - Termination

**Context:** Synchronous algorithm that must eventually terminate.

**Property:** The algorithm eventually reaches a terminal state.

**LTL Formula:**
```
F(terminated)
```

**CTL Formula:**
```
EF(terminated)
```

**Description:** In a synchronous system where all processes execute in lockstep, the algorithm must eventually reach a termination condition.

#### Use Case 2: Asynchronous System - Progress

**Context:** Asynchronous distributed algorithm making progress toward a goal.

**Property:** The system eventually reaches a goal state.

**LTL Formula:**
```
F(goal_reached)
```

**CTL Formula:**
```
EF(goal_reached)
```

**Description:** Despite asynchronous execution and potential delays, the system must eventually reach the goal state.

---

### Pattern: Recurrence (Infinitely Often)

**LTL:** `GF(φ)` - Globally, eventually φ (infinitely often)  
**CTL:** `AG(AF(φ))` - On all paths, globally, eventually φ holds

#### Use Case 1: Synchronous System - Fair Scheduling

**Context:** Round-robin scheduler in a synchronous system.

**Property:** Every process is scheduled infinitely often.

**LTL Formula:**
```
GF(process_1_scheduled) && GF(process_2_scheduled) && ... && GF(process_N_scheduled)
```

**CTL Formula:**
```
AG(AF(process_1_scheduled)) && AG(AF(process_2_scheduled)) && ... && AG(AF(process_N_scheduled))
```

**Description:** In a synchronous system with a fair scheduler, every process must be given execution time infinitely often.

#### Use Case 2: Asynchronous System - Heartbeat

**Context:** Distributed system with heartbeat messages.

**Property:** Heartbeat messages are sent infinitely often.

**LTL Formula:**
```
GF(heartbeat_sent)
```

**CTL Formula:**
```
AG(AF(heartbeat_sent))
```

**Description:** In an asynchronous distributed system, heartbeat messages must be sent repeatedly to maintain liveness monitoring.

---

### Pattern: Strong Until

**LTL:** `φ U ψ` - φ holds until ψ holds (and ψ eventually holds)  
**CTL:** `A(φ U ψ)` - On all paths, φ holds until ψ holds

#### Use Case 1: Synchronous System - Phase Transition

**Context:** Synchronous state machine with distinct phases.

**Property:** System remains in initialization phase until ready, then transitions to operational phase.

**LTL Formula:**
```
initialization U operational
```

**CTL Formula:**
```
A(initialization U operational)
```

**Description:** In a synchronous system, the initialization phase must persist until the system is ready, at which point it transitions to the operational phase.

#### Use Case 2: Asynchronous System - Resource Reservation

**Context:** Asynchronous system with resource reservation protocol.

**Property:** Resource remains reserved until it is released or used.

**LTL Formula:**
```
resource_reserved U (resource_released || resource_used)
```

**CTL Formula:**
```
A(resource_reserved U (resource_released || resource_used))
```

**Description:** In an asynchronous system, once a resource is reserved, it must remain reserved until it is either explicitly released or used.

---

## Reactiveness Properties

Reactiveness combines safety and liveness: the system must continuously respond to environment inputs.

### Pattern: Request-Response (Reactivity)

**LTL:** `G(request → F(response))` - Always, if request then eventually response  
**CTL:** `AG(request → AF(response))` - On all paths, always, if request then eventually response

#### Use Case 1: Synchronous System - Reactive Controller

**Context:** Synchronous reactive controller responding to sensor inputs.

**Property:** Every sensor reading triggers an eventual control action.

**LTL Formula:**
```
G(sensor_reading → F(control_action))
```

**CTL Formula:**
```
AG(sensor_reading → AF(control_action))
```

**Description:** In a synchronous reactive system, every sensor input must eventually result in a control output, ensuring the system remains responsive.

#### Use Case 2: Asynchronous System - Event Handler

**Context:** Asynchronous event-driven system.

**Property:** Every event is eventually processed by a handler.

**LTL Formula:**
```
G(event_occurred → F(event_handled))
```

**CTL Formula:**
```
AG(event_occurred → AF(event_handled))
```

**Description:** In an asynchronous event-driven system, all events must eventually be processed, ensuring the system remains reactive to external stimuli.

---

### Pattern: Conditional Liveness

**LTL:** `G(φ → GF(ψ))` - Always, if φ then infinitely often ψ  
**CTL:** `AG(φ → AG(AF(ψ)))` - On all paths, always, if φ then always eventually ψ

#### Use Case 1: Synchronous System - Periodic Task

**Context:** Synchronous system with periodic task execution.

**Property:** If a task is enabled, it executes infinitely often.

**LTL Formula:**
```
G(task_enabled → GF(task_executed))
```

**CTL Formula:**
```
AG(task_enabled → AG(AF(task_executed)))
```

**Description:** In a synchronous system, enabled tasks must execute repeatedly, ensuring periodic behavior.

#### Use Case 2: Asynchronous System - Fair Resource Access

**Context:** Asynchronous system with shared resources.

**Property:** If a process requests a resource, it eventually gets access infinitely often (under fairness).

**LTL Formula:**
```
G(resource_requested → GF(resource_granted))
```

**CTL Formula:**
```
AG(resource_requested → AG(AF(resource_granted)))
```

**Description:** In an asynchronous system with fair scheduling, resource requests must be granted repeatedly.

---

## GR(1) Patterns

GR(1) (Generalized Reactivity of rank 1) is a fragment of LTL that is efficiently realizable. It has the form:

```
(φ^e_1 ∧ ... ∧ φ^e_m) → (φ^s_1 ∧ ... ∧ φ^s_n)
```

Where:
- **Environment assumptions** (φ^e_i): Safety (G) and justice (GF) properties
- **System guarantees** (φ^s_i): Safety (G) and justice (GF) properties

### Pattern: Basic GR(1) Template

**Structure:**
```
(⋀ᵢ G(env_assumption_i) ∧ ⋀ⱼ GF(env_justice_j)) 
→ 
(⋀ₖ G(system_guarantee_k) ∧ ⋀ₗ GF(system_justice_l))
```

#### Use Case 1: Synchronous System - Mutual Exclusion with Fairness

**Context:** Two processes competing for a shared resource in a synchronous system.

**Environment Assumptions:**
- Safety: Process requests are mutually exclusive in each round
- Justice: Each process requests the resource infinitely often

**System Guarantees:**
- Safety: Only one process holds the resource at a time
- Justice: Each process gets the resource infinitely often

**GR(1) Formula:**
```
(G(!(request_1 && request_2)) ∧ GF(request_1) ∧ GF(request_2))
→
(G(!(grant_1 && grant_2)) ∧ GF(grant_1) ∧ GF(grant_2))
```

**Description:** In a synchronous system, if the environment ensures mutually exclusive requests and each process requests infinitely often, then the system guarantees mutual exclusion and fair resource allocation.

#### Use Case 2: Asynchronous System - Producer-Consumer with Bounded Buffer

**Context:** Asynchronous producer-consumer system with bounded buffer.

**Environment Assumptions:**
- Safety: Producer doesn't produce when buffer is full
- Justice: Producer produces infinitely often
- Justice: Consumer consumes infinitely often

**System Guarantees:**
- Safety: Buffer never overflows (buffer_count <= N)
- Safety: Consumer doesn't consume from empty buffer
- Justice: Produced items are eventually consumed

**GR(1) Formula:**
```
(G(buffer_full → !produce) ∧ GF(produce) ∧ GF(consume))
→
(G(buffer_count <= N) ∧ G(buffer_empty → !consume) ∧ GF(consume))
```

**Description:** In an asynchronous system, if the environment respects buffer capacity and both producer and consumer act infinitely often, the system guarantees bounded buffer operation and eventual consumption.

---

### Pattern: Conditional GR(1)

**Structure:** GR(1) with conditional guarantees based on environment state.

#### Use Case 1: Synchronous System - Conditional Service

**Context:** Synchronous service that responds conditionally to requests.

**Environment Assumptions:**
- Safety: Requests are valid (valid_request)
- Justice: Valid requests occur infinitely often

**System Guarantees:**
- Safety: Service only responds to valid requests
- Justice: Valid requests are eventually serviced

**GR(1) Formula:**
```
(G(valid_request) ∧ GF(valid_request))
→
(G(service_response → valid_request) ∧ GF(service_response))
```

**Description:** In a synchronous system, if the environment only sends valid requests infinitely often, the system guarantees responses only to valid requests and eventually services all valid requests.

#### Use Case 2: Asynchronous System - Adaptive Rate Control

**Context:** Asynchronous system with adaptive rate control based on load.

**Environment Assumptions:**
- Safety: Load signals are consistent
- Justice: High load occurs infinitely often
- Justice: Low load occurs infinitely often

**System Guarantees:**
- Safety: Rate is reduced when high load is detected
- Safety: Rate is increased when low load is detected
- Justice: System adapts to load changes

**GR(1) Formula:**
```
(G(load_signal_consistent) ∧ GF(high_load) ∧ GF(low_load))
→
(G(high_load → rate_reduced) ∧ G(low_load → rate_increased) ∧ GF(rate_adjusted))
```

**Description:** In an asynchronous system, if the environment provides consistent load signals and both high and low load occur infinitely often, the system adapts its rate accordingly.

---

## Counter Strategies for GR(1)

When a GR(1) property is unrealizable, counter strategies show how the environment can force the system to violate its guarantees.

### Pattern: Environment Counter Strategy

**Structure:** A strategy for the environment that demonstrates unrealizability by showing how environment assumptions can be satisfied while preventing system guarantees.

#### Use Case 1: Synchronous System - Unrealizable Mutual Exclusion

**Context:** Two processes with conflicting requirements in a synchronous system.

**Problem:** System cannot guarantee mutual exclusion if both processes always request simultaneously.

**Counter Strategy:**
- Environment always sets `request_1 = true` and `request_2 = true` in every round
- System must grant to at least one process (by liveness)
- But granting to both violates mutual exclusion (safety)
- System cannot satisfy both safety and liveness

**GR(1) Formula (Unrealizable):**
```
(G(true) ∧ GF(request_1) ∧ GF(request_2))
→
(G(!(grant_1 && grant_2)) ∧ GF(grant_1) ∧ GF(grant_2))
```

**Counter Strategy Description:**
1. Environment always enables both requests
2. System must eventually grant to process 1 (justice)
3. System must eventually grant to process 2 (justice)
4. But system cannot grant to both simultaneously (safety violation)

**Fix:** Add environment assumption: `G(!(request_1 && request_2))` - requests are mutually exclusive.

#### Use Case 2: Asynchronous System - Unrealizable Bounded Buffer

**Context:** Producer-consumer with insufficient buffer capacity.

**Problem:** System cannot guarantee bounded buffer if producer rate exceeds consumer rate.

**Counter Strategy:**
- Environment always enables production (GF(produce))
- Environment never enables consumption (violates GF(consume) assumption)
- System must accept all productions (by liveness)
- Buffer eventually overflows (safety violation)

**GR(1) Formula (Unrealizable):**
```
(GF(produce) ∧ G(!consume))
→
(G(buffer_count <= N) ∧ GF(consume))
```

**Counter Strategy Description:**
1. Environment produces infinitely often
2. Environment never consumes (violates assumption, but shows what happens)
3. System must accept productions (justice)
4. Buffer grows unbounded (safety violation)

**Fix:** Ensure environment assumption includes `GF(consume)` - consumer must act infinitely often.

---

### Pattern: System Counter Strategy

**Structure:** A strategy showing how the system can violate guarantees even when environment assumptions hold.

#### Use Case 1: Synchronous System - System Deadlock

**Context:** Synchronous system that can deadlock.

**Problem:** System can enter a state where no progress is possible.

**Counter Strategy:**
- Environment satisfies all assumptions
- System enters deadlock state where no transitions are enabled
- System cannot satisfy liveness guarantees

**GR(1) Formula (Unrealizable):**
```
(G(env_assumption) ∧ GF(env_justice))
→
(G(system_safety) ∧ GF(system_justice))
```

**Counter Strategy Description:**
1. Environment maintains assumptions
2. System reaches deadlock state
3. System cannot make progress (violates GF(system_justice))
4. System is stuck forever

**Fix:** Add system guarantee: `G(EF(progress))` - system can always make progress.

#### Use Case 2: Asynchronous System - Unfair Scheduling

**Context:** Asynchronous system with unfair scheduler.

**Problem:** System can starve some processes indefinitely.

**Counter Strategy:**
- Environment satisfies assumptions
- System scheduler always favors process A
- Process B is never scheduled
- System violates fairness guarantee

**GR(1) Formula (Unrealizable):**
```
(G(env_assumption) ∧ GF(env_justice))
→
(G(system_safety) ∧ GF(process_A_scheduled) ∧ GF(process_B_scheduled))
```

**Counter Strategy Description:**
1. Environment maintains assumptions
2. System always schedules process A
3. Process B is never scheduled (violates GF(process_B_scheduled))
4. System violates fairness

**Fix:** Implement fair scheduling algorithm or add stronger fairness guarantees.

---

## Beyond GR(1): Recurrence after Stability (Σ₃)

The patterns above all sit at alternation depth ≤ 2 in their mu-calculus normal form — that's the GR(1) cap. Real synthesis problems sometimes need more, and mununu's `parity-game` controller mode is the only mode that handles arbitrary alternation correctly.

### Pattern: Recurrence after Stability

A recurrence obligation that becomes active *only after* a stability event has occurred. Once the system reaches a stable region, a separate recurrence property must hold forever within that region — but the adversary controls when (or whether) the stable region is entered.

**LTL:**
```
G(req → F(ack ∧ G(retry → F done)))
```

In English: every request must eventually be acknowledged, and from that ack onward, every retry must eventually be answered by `done` — forever after the first ack.

**Mu-calculus translation** (NNF, with `F→μ` and `G→ν`):
```
νZ. ((¬req ∨ μY. ((ack ∧ νX. ((¬retry ∨ μW. (done ∨ ◇W)) ∧ □X)) ∨ ◇Y)) ∧ □Z)
```

**Alternation depth:** 4. Reading from outside in: `νZ → μY → νX → μW`, three strict alternations. This is genuinely above GR(1) — it does not collapse to a Σ₂ disjunction the way `GFp → GFq` does, because the "forever after" obligation is conditional on *having entered* the post-ack region.

**Provenance:** Manna-Pnueli temporal hierarchy (Σ₃-complete reactivity class). Industrial appearance in SYNTCOMP TLSF benchmarks `lily-demo` and `amba_decomposed_lock_*`.

**Worked example in mununu:** [`examples/bus_arbiter_retry.ctxdsl`](../../examples/bus_arbiter_retry.ctxdsl) — 4-state bus arbiter where the controller must `ack` requests, then handle `retry` events with `done` forever after the first ack, with the option to `release` back to idle. Use `--controller-mode parity-game` for theoretical correctness; lower modes happen to give the same realizability verdict on this small system but their strategies aren't proven correct in general at alt > 2.

```bash
mununu context synth examples/bus_arbiter_retry.ctxdsl \
    --formula recurrence_after_stability --automaton BusArbiter \
    --controller-mode parity-game
```

**Dual-channel extension:** [`examples/dual_arbiter_alt4.ctxdsl`](../../examples/dual_arbiter_alt4.ctxdsl) composes two independent recurrence-after-stability obligations (one per channel). The controller must (a) rotate between two liveness goals — *memory of which obligation is being served* — and (b) disable some controllable transitions to commit to a strategy. The functional mode synthesizes 31 transitions vs the projection's 48, an explicit demonstration of "the controller disables 17 transitions". Property body remains alt 4 (conjunction doesn't increase alternation).

See [Controller Modes](https://github.com/your-org/mununu/wiki/Controller-Modes) on the wiki for the full mode comparison.

---

## Pattern Summary Table

| Pattern | LTL | CTL | Synchronous Use Case | Asynchronous Use Case |
|---------|-----|-----|---------------------|----------------------|
| **Invariant** | `G(φ)` | `AG(φ)` | Mutual exclusion | Bounded buffer |
| **Response** | `G(φ → F(ψ))` | `AG(φ → AF(ψ))` | Request-response | Message delivery |
| **Eventually** | `F(φ)` | `EF(φ)` | Termination | Progress |
| **Recurrence** | `GF(φ)` | `AG(AF(φ))` | Fair scheduling | Heartbeat |
| **Until** | `φ U ψ` | `A(φ U ψ)` | Phase transition | Resource reservation |
| **Reactivity** | `G(φ → F(ψ))` | `AG(φ → AF(ψ))` | Reactive controller | Event handler |
| **Conditional Liveness** | `G(φ → GF(ψ))` | `AG(φ → AG(AF(ψ)))` | Periodic task | Fair resource access |
| **Recurrence after Stability** (Σ₃, alt 4) | `G(req → F(ack ∧ G(retry → F done)))` | — | Bus arbiter with retry | Stateful protocol with delayed obligation |
| **GR(1) Basic** | `(⋀G(φᵢ) ∧ ⋀GF(φⱼ)) → (⋀G(ψₖ) ∧ ⋀GF(ψₗ))` | N/A | Mutual exclusion with fairness | Producer-consumer |
| **GR(1) Conditional** | Conditional GR(1) | N/A | Conditional service | Adaptive rate control |

---

## Notes on Synchronous vs Asynchronous Systems

### Synchronous Systems
- **Characteristics:** All processes execute in lockstep, global clock, deterministic interleaving
- **Pattern Usage:** Properties often relate to round-based execution, phase transitions, and deterministic scheduling
- **Verification:** Easier to verify due to deterministic execution paths

### Asynchronous Systems
- **Characteristics:** Independent process execution, no global clock, non-deterministic interleaving
- **Pattern Usage:** Properties often relate to message passing, fairness, and eventual consistency
- **Verification:** More complex due to interleaving, often requires fairness assumptions

---

## References

- **LTL (Linear Temporal Logic):** Pnueli, A. (1977). "The temporal logic of programs"
- **CTL (Computation Tree Logic):** Clarke, E. M., & Emerson, E. A. (1981). "Design and synthesis of synchronization skeletons using branching time temporal logic"
- **GR(1):** Piterman, N., Pnueli, A., & Sa'ar, Y. (2006). "Synthesis of reactive(1) designs"
- **Reactive Systems:** Harel, D., & Pnueli, A. (1985). "On the development of reactive systems"

---

## Appendix: Common Formula Patterns

### Safety Patterns
```ltl
G(!bad_state)                    // Never reach bad state
G(φ → X(ψ))                      // If φ then next ψ
G(φ → (ψ W χ))                   // If φ then ψ until χ
```

### Liveness Patterns
```ltl
F(goal_state)                     // Eventually reach goal
GF(φ)                             // Infinitely often φ
G(φ → F(ψ))                       // Always, if φ then eventually ψ
```

### Reactiveness Patterns
```ltl
G(request → F(response))          // Request-response
G(φ → GF(ψ))                      // Conditional recurrence
G(φ → (ψ U χ))                    // Conditional until
```

### GR(1) Template
```ltl
(⋀ᵢ G(env_safety_i) ∧ ⋀ⱼ GF(env_justice_j))
→
(⋀ₖ G(sys_safety_k) ∧ ⋀ₗ GF(sys_justice_l))
```
