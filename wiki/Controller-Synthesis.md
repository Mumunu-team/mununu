# Controller Synthesis

> **Alpha Software** — Mununu is under active development. APIs, syntax, and behavior may change. We welcome feedback and bug reports via [GitHub Issues](https://github.com/Mumunu-team/mununu/issues).

## What is Controller Synthesis?

Controller synthesis answers the question: **"Can we automatically build a controller that enforces a given property, no matter what the environment does?"** Instead of manually designing a state machine and then verifying it, you describe the plant (the system to be controlled), declare which actions are under your control, specify the desired property, and let Mununu compute a controller that is correct by construction.

For hardware designers, this is analogous to automatic logic synthesis from temporal specifications -- but at the protocol and control-flow level. You describe the allowable behaviors of your bus arbiter, handshake protocol, or state machine, and Mununu computes the most permissive controller that satisfies your safety or liveness requirements.

## Controllable vs. Uncontrollable Actions

Mununu follows the **Ramadge-Wonham supervisory control** framework. Every action (label) in the system is classified as either:

- **Controllable**: actions the system/controller can choose to enable or disable. These represent outputs, actuator commands, and decisions the designer controls. Examples: motor commands, grant signals, valve open/close.
- **Uncontrollable**: actions the environment triggers and the controller cannot prevent. These represent inputs, sensor events, and external stimuli. Examples: sensor readings, client requests, fault signals.

The key invariant: **the controller can only restrict controllable actions.** Uncontrollable actions always pass through -- if the environment can trigger a transition, the controller cannot block it. A valid controller must tolerate every possible uncontrollable event while ensuring the property holds.

## CTXDSL Syntax

### Declaring Controllable Actions

Inside an automaton block, the `controllable` section lists labels under the controller's authority. All other labels in the alphabet are implicitly uncontrollable:

```
automaton RobotArm {
    controllable {
        label extend;
        label retract;
        label grip;
        label release;
    }
    // object_detected, obstacle_detected, idle are uncontrollable
    ...
}
```

### Declaring a Controller

Controllers are declared in the `controllers` block. Each controller references a source automaton and a formula it must satisfy:

```
controllers {
    controller safe_robot {
        source RobotArm;
        satisfying safety_invariant;
    }
}
```

### Controller Options

Controllers support optional configuration for minimization and diagnostics:

```
controllers {
    controller impossible_valve {
        source Valve;
        satisfying impossible_spec;
        minimize = true;
        diagnostics {
            counterexample = true;
            deadlock_traces = true;
        }
    }
}
```

## Realizability

A specification is **realizable** when there exists a controller strategy that satisfies the property for all possible environment behaviors. A specification is **unrealizable** when no such controller exists -- the environment has a winning counterstrategy that forces the property to be violated.

Realizability depends on three factors:
1. The structure of the plant (states and transitions)
2. The controllability partition (which actions the controller owns)
3. The property formula (what must be satisfied)

### When a Controller Exists

A controller exists when the initial state(s) of the automaton fall within the **winning region** -- the set of states from which the controller has a strategy to enforce the property indefinitely. Mununu computes this winning region using mu-calculus fixpoint evaluation with game semantics.

### When a Controller Does Not Exist

Synthesis fails when the initial state lies outside the winning region. This means the environment can force a violation from the start. Mununu reports this as unrealizable and can optionally provide diagnostic information (counterexamples and deadlock traces) to help understand why.

## Example: Robot Arm (Tutorial 06)

The robot arm has controllable actuators and uncontrollable sensors:

```
context robot_arm {
    alphabet {
        label extend; label retract; label grip; label release;
        label object_detected; label obstacle_detected; label idle;
    }

    automata {
        automaton RobotArm {
            controllable {
                label extend; label retract;
                label grip;   label release;
            }

            states {
                state Home initial;
                state Extended; state Gripping;
                state Retracting; state Blocked;
            }

            transitions {
                transition Home -> Extended on label extend;
                transition Extended -> Gripping on label grip;
                transition Gripping -> Retracting on label retract;
                transition Retracting -> Home on label release;
                transition Extended -> Gripping on label object_detected;
                transition Extended -> Blocked on label obstacle_detected;
                transition Blocked -> Home on label retract;
                transition Home -> Home on label idle;
                transition Extended -> Extended on label idle;
            }
        }
    }

    mu_formulas {
        formula safety_invariant {
            over RobotArm;
            body = nu X. ([] X);
        }

        formula ctrl_return_home {
            over RobotArm;
            body = mu X. (Home || [ (ctrl = controllable) ] X);
        }
    }

    controllers {
        controller safe_robot {
            source RobotArm;
            satisfying safety_invariant;
        }
    }
}
```

The `safety_invariant` formula (`nu X. ([] X)`) is realizable -- the controller can keep the system in valid states. The `ctrl_return_home` formula uses game semantics: `[ (ctrl = controllable) ] X` means all uncontrollable transitions must stay in the winning region AND at least one controllable transition must progress toward `Home`.

**Synthesis output** (via CLI):

```bash
mununu context synth tutorial/examples/06_controllability.ctxdsl \
    --formula safety_invariant --automaton RobotArm
```

The controller restricts reachable states to the winning region while allowing all safe controllable actions.

## Unrealizable Example: Impossible Valve (Tutorial 09)

When a specification is physically impossible, synthesis reports unrealizability:

```
context unrealizable_valve {
    automata {
        automaton Valve {
            controllable { label open_valve; label close_valve; }
            states { state Closed initial; state Open; }
            transitions {
                transition Closed -> Open on label open_valve;
                transition Open -> Closed on label close_valve;
                transition Closed -> Closed on label pressure_drop;
                transition Open -> Open on label pressure_rise;
            }
        }
    }

    mu_formulas {
        formula impossible_spec {
            over Valve;
            body = Open && Closed;
        }
    }

    controllers {
        controller impossible_valve {
            source Valve;
            satisfying impossible_spec;
            diagnostics {
                counterexample = true;
                deadlock_traces = true;
            }
        }
    }
}
```

No state can satisfy `Open && Closed` simultaneously -- the formula evaluates to the empty set. The winning region is empty, so the controller is unrealizable from any initial state. With `diagnostics` enabled, Mununu reports counterexample traces showing that the environment wins from every state.

## GR(1) Fix: Fairness Assumptions

When a simple liveness property is unrealizable because the environment can stall, adding **fairness assumptions** in GR(1) form can make it realizable. The GR(1) pattern is:

```
GF(assumption) -> GF(guarantee)
```

In mu-calculus, this becomes:

```
(! (nu X. (mu Y. (assumption || <> Y)) && ([] X)))
    || (nu Z. (mu W. (guarantee || <> W)) && ([] Z))
```

**Example: AMBA Bus Arbiter** -- If client 0 requests infinitely often (fairness assumption), it must be granted infinitely often (liveness guarantee):

```
formula grant0_reachable {
    over bus_system;
    body = (! (nu X. (mu Y. (Requesting0 || <> Y)) && ([] X)))
        || (nu Z. (mu W. (Grant0 || <> W)) && ([] Z));
}
```

This says: if the environment is fair (GF Requesting0), then the system must be fair in response (GF Grant0). Without the fairness assumption, the environment could starve a client by never requesting, making the guarantee vacuously impossible to verify.

## Controller Minimization

When `minimize = true` is set (either in CTXDSL or via `--minimize` on the CLI), Mununu applies bisimulation reduction to the synthesized controller. This produces the smallest controller with equivalent behavior, which is useful for:

- Reducing implementation complexity in hardware
- Generating more readable controller automata
- Comparing controllers structurally

```
controllers {
    controller minimal_arbiter {
        source Arbiter;
        satisfying safety_invariant;
        minimize = true;
    }
}
```

## Diagnostics

When synthesis fails (unrealizable), the API response automatically includes a **counterstrategy graph** showing the environment's winning strategy as Cytoscape-compatible elements. The graph only includes states reachable from initial states via the transitions kept by strategy extraction, filtering out orphaned states.

Additional diagnostics can be enabled:

| Option | CLI Flag | Purpose |
|--------|----------|---------|
| `counterexample = true` | `--counterexample` | Generate counterexample and lasso traces showing how the environment forces violation |
| `deadlock_traces = true` | `--deadlock-traces` | Capture traces that lead to deadlock states (no outgoing transitions in the controller) |

```bash
mununu context synth spec.ctxdsl \
    --formula impossible_spec --automaton Valve \
    --counterexample --deadlock-traces
```

## TLSF/AIGER Turn-Based Encoding

When synthesizing from TLSF or AIGER specifications, the adapter produces a **turn-based compound-label encoding** that correctly models Mealy game semantics:

- **Compound labels**: `env_{bits}` (uncontrollable) and `ctrl_{bits}` (controllable) replace per-signal `set_`/`clr_` labels. Each label atomically commits all signals for one player.
- **Turn bit**: States include a turn bit. At `turn=0` (env's turn), only uncontrollable `env_*` transitions exist. At `turn=1` (ctrl's turn), only controllable `ctrl_*` transitions exist.
- **Skolem alternation**: `[(ctrl=Controllable)]` at `turn=0` states is universal (∀ env transitions). At `turn=1` states it is existential (∃ ctrl transition). This correctly implements Mealy games: for every environment move, the controller can find a response.
- **Formulas**: Emitted as mu-calculus (not LTL) with `(turn || φ)` guards that skip propositional evaluation at intermediate (`turn=1`) states. This prevents false unrealizable verdicts from formulas being checked before the controller has responded.

This encoding doubles the state space (2^(N+1) vs 2^N) but produces semantically correct synthesis verdicts for all SYNTCOMP Lily benchmarks.

## Known Limitations

### Strategy vs Projection

Current controllers are **projections** of the winning region — they retain all transitions between winning states. For safety properties, this IS a valid strategy (any move within the region preserves the invariant). For liveness/GR(1), the controller may include transitions that don't guarantee progress.

Use `--extract-strategy` to produce a **positional strategy** that keeps only ONE controllable transition per state:

```bash
mununu context synth example.ctxdsl --formula F --automaton A --extract-strategy
```

### GR(1) Obligation Tracking

Mununu evaluates GR(1) formulas as nested mu-calculus fixpoints, which correctly computes the winning region. However, it does **not** implement the Piterman-Pnueli-Sa'ar rank-based algorithm that tracks which obligation is being pursued. This means:

- The winning region is correct
- But the synthesized controller does not cycle through obligations (g₁ → g₂ → ... → gₘ → repeat)
- For `[]<> g₁ && []<> g₂`, the controller knows WHICH states are winning but not HOW to schedule visits

This is planned for a future release via instrumented fixpoint evaluation (Bruse, Friedmann & Lange, 2016).

### Lasso Traces

Counterexample traces for liveness properties include **lasso format** with transition labels:

```
lasso trace #0: Red -[timer_tick]-> PedWaiting -[ped_wait]-> (PedWaiting)^ω
```

The prefix shows the path to the cycle entry, each arrow is annotated with the transition label taken, and the cycle repeats infinitely. The last label in the cycle is the transition from the last cycle state back to the first (the closing edge of the omega loop).

Use `--counterexample` with synthesis to see lasso traces. In the API, the `LassoTrace` object includes `prefix_labels` and `cycle_labels` arrays parallel to the `prefix` and `cycle` state arrays.

## See Also

- [LTL Properties](LTL-Properties.md) -- writing temporal specifications
- [Counterstrategy](Counterstrategy.md) -- understanding formula inversion and environment winning strategies
- [Hardware Verification Patterns](Hardware-Verification-Patterns.md) -- synthesis examples for common hardware protocols
- [CLI Reference](CLI-Reference.md) -- full `synth` command documentation
