# Composition

> **Alpha Software** — Mununu is under active development. APIs, syntax, and behavior may change. We welcome feedback and bug reports via [GitHub Issues](https://github.com/Mumunu-team/mununu/issues).

Composition is how you build larger systems from smaller automata. Mununu supports two modes -- synchronous and asynchronous -- and lets you nest them for hierarchical, cross-domain designs.

## Synchronous Composition

In synchronous composition, **all members step simultaneously on every cycle**. Shared labels (labels that appear in more than one member) must fire together. Non-shared labels combine freely when both sides move.

```
composition {
    synchronous pipeline {
        members [Producer, Consumer];
    }
}
```

### Skip Labels

Because every member must take a step on every cycle, each automaton needs its own **skip label** -- a self-loop that lets it do nothing while the other side acts. If two automata share the same idle/skip label, they are forced to idle together, which is usually not what you want.

```
automaton Producer {
    // ...
    transitions {
        transition Empty -> Empty on label p_skip;
        transition DataReady -> DataReady on label p_skip;
        // ...
    }
}

automaton Consumer {
    // ...
    transitions {
        transition Waiting -> Waiting on label c_skip;
        transition Processing -> Processing on label c_skip;
        // ...
    }
}
```

### Example: Producer-Consumer Pipeline

A producer generates data and hands it to a consumer via a shared `transfer` label. Both run on the same clock.

```
context sync_producer_consumer {
    automata {
        automaton Producer {
            controllable { label produce; }
            states {
                state Empty initial;
                state DataReady;
            }
            transitions {
                transition Empty -> DataReady on label produce;
                transition DataReady -> Empty on label transfer;
                transition Empty -> Empty on label p_skip;
                transition DataReady -> DataReady on label p_skip;
            }
        }

        automaton Consumer {
            controllable { label consume; }
            states {
                state Waiting initial;
                state Processing;
            }
            transitions {
                transition Waiting -> Processing on label transfer;
                transition Processing -> Waiting on label consume;
                transition Waiting -> Waiting on label c_skip;
                transition Processing -> Processing on label c_skip;
            }
        }
    }

    composition {
        synchronous pipeline {
            members [Producer, Consumer];
        }
    }
}
```

## Asynchronous Composition

In asynchronous composition, **any single member can step independently** while the others stay still. This models interleaved execution -- no shared clock, no lock-step requirement. Shared labels still synchronize when they fire.

```
composition {
    asynchronous sensor_network {
        members [SensorA, SensorB, Monitor];
    }
}
```

No skip labels are needed because members are not forced to move on every step.

### Example: Sensor Network

Two vibration sensors operate independently. A central monitor receives their reports.

```
context async_sensors {
    automata {
        automaton SensorA {
            controllable { label sensor_a_sample; }
            states {
                state IdleA initial;
                state SampledA;
            }
            transitions {
                transition IdleA -> SampledA on label sensor_a_sample;
                transition SampledA -> IdleA on label sensor_a_report;
            }
        }

        automaton SensorB {
            controllable { label sensor_b_sample; }
            states {
                state IdleB initial;
                state SampledB;
            }
            transitions {
                transition IdleB -> SampledB on label sensor_b_sample;
                transition SampledB -> IdleB on label sensor_b_report;
            }
        }

        automaton Monitor {
            controllable { label monitor_process; }
            states {
                state Ready initial;
                state ReceivedA;
                state ReceivedB;
            }
            transitions {
                transition Ready -> ReceivedA on label sensor_a_report;
                transition Ready -> ReceivedB on label sensor_b_report;
                transition ReceivedA -> Ready on label monitor_process;
                transition ReceivedB -> Ready on label monitor_process;
            }
        }
    }

    composition {
        asynchronous sensor_network {
            members [SensorA, SensorB, Monitor];
        }
    }
}
```

## Hierarchical Composition

You can nest compositions: compose groups synchronously first, then combine those groups asynchronously (or vice versa). This is the natural way to model multi-clock-domain hardware.

### Example: Cross-Domain ADC Pipeline

A fast sampling domain (ADC + buffer) runs lock-step on a fast clock. A slow processing domain (processor + result store) runs lock-step on a slow clock. The two domains interleave independently, communicating through a shared FIFO label.

```
context cross_domain_adc {
    automata {
        // Fast domain: ADC and SampleBuffer (each with skip labels)
        automaton ADC { /* ... */ }
        automaton SampleBuffer { /* ... */ }

        // Slow domain: Processor and ResultStore (each with skip labels)
        automaton Processor { /* ... */ }
        automaton ResultStore { /* ... */ }
    }

    composition {
        // Step 1: compose each clock domain synchronously
        synchronous fast_domain {
            members [ADC, SampleBuffer];
        }
        synchronous slow_domain {
            members [Processor, ResultStore];
        }

        // Step 2: compose the two domains asynchronously
        asynchronous system {
            members [fast_domain, slow_domain];
        }
    }
}
```

The full example is in `tutorial/examples/04_cross_domain.ctxdsl`.

## Synchronous vs. Asynchronous: When to Use Each

| | Synchronous | Asynchronous |
|---|---|---|
| **Execution model** | Lock-step; all members move every cycle | Interleaved; one member moves per step |
| **Shared labels** | Must fire simultaneously | Must fire simultaneously (when chosen) |
| **Skip labels** | Required (per-automaton) | Not needed |
| **Typical use** | Components on the same clock domain | Independent subsystems, different clocks |
| **State space** | Product of member states (dense) | Product of member states (sparser in practice) |
| **Example** | ADC + sample buffer on a fast clock | Two sensors reporting to a monitor |

## Key Gotchas

- **Shared idle labels force mutual idling.** In synchronous mode, if two automata share the same skip/idle label, they can only idle together. Always use per-automaton skip labels (e.g., `p_skip`, `c_skip`).
- **Skip labels are only needed for synchronous composition.** In asynchronous mode, members are not required to step, so self-loop skip labels are unnecessary.
- **Hierarchical composition is order-sensitive.** Inner compositions must be declared before the outer composition that references them.
- **Formulas can target any level.** You can write mu-calculus properties over individual automata, inner compositions, or the outermost system -- just set the `over` field accordingly.
- **A shared label the component cannot take is a label the environment cannot emit — silently.** In synchronous composition, if only one side declares a transition on a shared label, the composed system can only fire it when *both* sides are ready. From the component's point of view this makes an unmodelled state look like "the environment does not act here" — but in the real system the environment *does* act; the event just gets dropped by the component. That is a common protocol-modelling footgun: an omitted transition constrains the environment rather than the component, and a liveness property that should have failed will hold vacuously. If the environment can act anyway and you lose the event, model the loss explicitly — a self-loop or a discard transition on the receiving side is the difference between *cannot happen* (a spec claim) and *happens and is lost* (a real bug). Filed against a real monono debug in mununu#477.
- **An action modelled as one transition asserts that every resource it uses is held, unchanged, for exactly that step.** A `fill` label meaning "land this row into the stage" is one transition, so nothing can happen *during* it — a payload that changes mid-fill is not a behaviour the model failed to explore, it is one the model declared impossible. **If the RTL can do anything between the start and the end of your label, your label is wrong.** The remedy is a pattern, not a feature: any resource with a handshake wants `acquire → use* → release` as separate labels, with the payload's identity carried in the automaton's *state* across the `use*`. Worked contrast: [`docs/ctxdsl-modelling-guide.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/ctxdsl-modelling-guide.md) Part II §A. Filed against a real monono debug in mununu#497.
- **`N = 1` is not an abstraction of `N` — it is a different system.** Collapsing a multiplicity to one slot deletes the entire class of "two parties disagree about which one", which is most of what a shared-resource model is *for*. It feels like ordinary abstraction (the same move as dropping a data width) and is not. **`N = 2` is the smallest honest number wherever identity matters.** See [`docs/ctxdsl-modelling-guide.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/ctxdsl-modelling-guide.md) Part II §B (mununu#497).

## KMTS Composition — Modality Merge (post-R.1)

> **Source of truth:** [`composition::compose`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/composition/mod.rs), [`Transition::modality`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/clts/mod.rs) — surface: CLI+API+UI.

The synchronous / asynchronous / superset semantics above apply unchanged to Kripke Modal Transition Systems (KMTSes — see [`docs/design/kmts-theory.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/kmts-theory.md)). What KMTS composition *adds* is **per-axis modality merging**: each transition carries a `TransitionModality` (`Sharp` for both may and must, `MayOnly` for over-approximation only), and the composition operator merges modalities pointwise on each axis.

### Modality merge rule

Each KMTS transition has two independent capabilities: `may` (the transition is admitted by the abstraction) and `must` (a concrete witness for the transition exists). Standard KMTS enforces `must ⊆ may`, so the valid capability sets are `{may}` (= `MayOnly`) and `{may, must}` (= `Sharp`). A composed transition has capability `c` iff **both** sides have a transition with capability `c` on the synchronizing label:

```text
has_may (left ⊗ right) = has_may (left) ∧ has_may (right)
has_must(left ⊗ right) = has_must(left) ∧ has_must(right)
```

Set-intersection on each axis, independently. The merge table is a corollary:

| `left.modality` | `right.modality` | `composed.modality` |
|---|---|---|
| `Sharp` | `Sharp` | `Sharp` (both have may; both have must) |
| `Sharp` | `MayOnly` | `MayOnly` (both have may; only one has must → composed has may but not must) |
| `MayOnly` | `MayOnly` | `MayOnly` (both have may; neither has must) |

Three cases, not six — the standard-KMTS invariant `must ⊆ may` eliminates a hypothetical `MustOnly` variant.

### Why this is the structural free lunch

Composition is purely structural (no SMT at compose time); refinement is congruential (Larsen–Larsen–Wąsowski FoSSaCS 2007 — refining one module's KMTS refines the composed KMTS without recomposition). The compositional verification problem reduces to the per-module abstraction problem, **without** an assume-guarantee discharge step.

### What this preserves and what it doesn't

**Preserves:** Per-module abstraction; cross-module verdict soundness *under per-module abstractions*; liveness reasoning under fairness; all CTLS-observable behaviour for Sharp-everywhere KMTSes (the legacy case is a structural special case).

**Does not preserve:** *Tightness* — the composed KMTS may have more `KleeneBot` verdicts than a monolithic predicate abstraction of the flattened system. The architecture doc [§7.2 worked counterexample](https://github.com/Mumunu-team/mununu/blob/main/docs/design/native-sv-abstraction.md#§7-compositional-kmts-the-structural-free-lunch) walks through a producer/consumer pair where the composed KMTS returns `KleeneBot` on a safety property because the per-module predicate sets lack a cross-module port-equality predicate; adding `data_eq_on_handshake = (valid ⇒ producer.data_out == consumer.data_in)` to the multi-module sidecar closes the gap.

The KMTS lifter auto-emits canonical port-equality predicates for every declared multi-module connection (`from: "producer.data_out", to: "consumer.data_in"` → `data_eq_on_handshake`). Authors only need to add predicates manually for arbitrated buses, stateful intermediates, and cross-domain CDC bridges.

### How this affects the existing examples

For Sharp-everywhere KMTSes (the case the existing examples above all describe — XState, microcode, ctxdsl, agentic adapters all produce `Sharp` transitions exclusively), the modality merge is `Sharp ⊗ Sharp = Sharp` on every composed transition. The 3-valued evaluator (`KleeneDomain`) reduces to the 2-valued one (`BoolDomain`) on a Sharp-everywhere KMTS: `KleeneBot` never appears in the verdict. So the existing producer-consumer and sensor-network examples are unchanged — the new modality machinery is *additive*, not a behavioural change for adapters that don't produce `MayOnly` transitions.

The KMTS lifter (SV / BTOR2 path, post-R.2) is the adapter that produces `MayOnly` transitions, because predicate abstraction creates over-approximation edges that no concrete witness backs. See [`docs/design/native-sv-abstraction.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/native-sv-abstraction.md) §6 for the design and [`docs/design/predicate-abstraction-recipe.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/predicate-abstraction-recipe.md) §3 for the predicate-image computation that decides which transitions are `Sharp` vs `MayOnly`.

## See Also

- [Verify Project Flow](Verify-Project-Flow) — composition's place in the verify pipeline; the KMTS pipeline highlights.
- [`docs/design/kmts-theory.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/kmts-theory.md) — KMTS theory, refinement preorder, 3-valued mu-calculus semantics, preservation theorem.
- [`docs/design/native-sv-abstraction.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/native-sv-abstraction.md) §6.5 (composition modality merge) + §7 (compositional KMTS).
- [`docs/abstraction.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/abstraction.md) — the canonical KMTS recipe and the legacy primitives.
