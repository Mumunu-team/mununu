# CTXDSL Language Reference

> **Alpha Software** — Mununu is under active development. APIs, syntax, and behavior may change. We welcome feedback and bug reports via [GitHub Issues](https://github.com/Mumunu-team/mununu/issues).

CTXDSL is Mununu's domain-specific language for defining automata, compositions, properties, and controllers in a single file. This page is the complete syntax reference.

## File Structure

Every `.ctxdsl` file has one top-level `context` block:

```
context my_system {
    alphabet { ... }
    constants { ... }
    ranges { ... }
    enums { ... }
    automata { ... }
    composition { ... }
    mu_formulas { ... }
    controllers { ... }
}
```

All sections are optional. Order does not matter.

## Alphabet

The `alphabet` block declares the signal/event labels used across automata. Labels declared here are available to all automata in the context.

```
alphabet {
    label req_assert;
    label req_deassert;
    label ack_assert;
    label ack_deassert;
}
```

You can also give a label a display name (used in graph visualization):

```
alphabet {
    label sclk = "SCLK";
}
```

Labels that appear only inside an automaton's `transitions` or `controllable` block are implicitly added to the alphabet -- you do not need to list them in `alphabet` unless you want to set a display name or make dependencies explicit.

## Constants and Ranges

Constants and ranges are declared at the context level, before or after automata.

```
const MAX_RETRY = 3;
range counter = 0 ..= MAX_RETRY;
```

Constants are `i64` values. Ranges define inclusive integer intervals and can reference constants in their bounds.

## Enums

Enum types define named finite value sets. Variants are mapped to integers (0, 1, 2, ...) at realization time.

```
enums {
    enum Status { idle, active, error };
    enum MsgType { req, ack, nack };
}
```

Enum types can be used as variable types in automata. Guards and effects reference variants by name:

```
variables {
    var mode: Status = idle;
}

transitions {
    transition S0 -> S1 on label activate
        guard mode == idle
        effects { mode = active; };
}
```

Under the hood, enum variables are desugared to `i64` and unrolled like any other integer variable.

## Automaton

An automaton is a labeled transition system with states, transitions, and optional controllability and variable declarations.

```
automata {
    automaton SpiMaster {
        controllable { ... }
        variables { ... }
        states { ... }
        transitions { ... }
    }
}
```

### States

One state must be marked `initial`. State names are identifiers that double as atomic propositions in mu-calculus formulas.

```
states {
    state Idle initial;
    state Selected;
    state Shifting;
    state Complete;
}
```

#### Per-state valuations (display metadata)

A state may carry a `valuations { … }` block with structured `key = value;` pairs. Values are integer literals or identifiers. The model checker ignores them; the trace renderer and the graph view print them under the state name as `{key1=val1, key2=val2}`.

Use this to hand-author examples that display the same per-state valuations adapter-driven flows (BTOR2, SV-yosys, extraction) inject through the side-channel:

```
states {
    state Green initial {
        valuations {
            is_red = 0;
            is_green = 1;
            is_yellow = 0;
            phase = green;
        }
    };
    state Yellow {
        valuations {
            is_red = 0;
            is_green = 0;
            is_yellow = 1;
            phase = yellow;
        }
    };
}
```

The block lives inside the optional outer state block; it can coexist with a `vars { … }` block in any order. Reserved-keyword names (e.g. `state`, `on`, `group`) are accepted as keys so the round-trip with adapter-emitted CTXDSL is safe. See [`examples/hw/traffic_light_valuations.ctxdsl`](https://github.com/Mumunu-team/mununu/blob/main/examples/hw/traffic_light_valuations.ctxdsl) for the canonical worked example.

#### Per-state 3-valued (Kleene) predicates

> Source of truth: [`Clts::with_3valued_predicate`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/clts/mod.rs) + [`parse_three_valued_pair`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/context_dsl/parser.rs) — surface: CLI+API+UI (the realized CLTS evaluates under any surface).

A state may carry a `predicates_3v { … }` block with `predicate = tristate;` pairs, where the tristate is `true`, `false`, or `unknown`. Unlike the 2-valued `predicates` block (which asserts a predicate *holds* at a state), these carry a full Kleene verdict and realize into the CLTS's `state_3valued_predicates` map (`KleeneT` / `KleeneF` / `KleeneBot`) — the round-trippable surface for a predicate-cube KMTS produced by the BTOR2 predicate-abstraction lift.

```
states {
    state cube_0 initial {
        predicates_3v {
            boot_fsm_is_idle = true;
            boot_fsm_is_done = false;
            counter_overflow = unknown;
        }
    };
}
```

The block lives inside the optional outer state block and coexists with `vars { … }` / `valuations { … }` in any order. `unknown` ⇒ `KleeneBot` (the abstraction is too coarse to decide the predicate at that cube — the CEGAR loop refines it).

### Controllable

The `controllable` block lists labels that the controller is allowed to enable or disable. Labels not listed here are treated as **uncontrollable** (environment actions that the controller cannot prevent).

```
controllable {
    label ack_assert;
    label ack_deassert;
    label latency_tick;
}
```

This distinction is central to controller synthesis. See the [Mu-Calculus Reference](Mu-Calculus-Reference) for controllability-aware modalities.

### Parameters (Process Templates)

An automaton can be parameterized over a range. The tool expands it into multiple concrete instances at realization time.

```
ranges {
    range Clients = 0 ..= 1;
}

automata {
    automaton Client {
        parameters {
            param i in Clients;
        }
        controllable { label req[i]; }
        states { state Idle initial; state Waiting; }
        transitions {
            transition Idle -> Waiting on label req[i];
            transition Waiting -> Idle on label grant[i];
        }
    }
}
```

This expands into `Client_0` and `Client_1`, each with their own labels (`req_0`/`grant_0` and `req_1`/`grant_1`). Composition members reference instances with index syntax: `members [Client[0], Client[1], Arbiter];`.

Currently only a single parameter per automaton is supported.

### State Groups & Wildcards

State groups name a set of states for use in bulk transition declarations. Wildcards match all declared states.

```
state_groups {
    group error_states = { ErrorMinor, ErrorMajor };
}

transitions {
    // Reset from any error state
    transition group error_states -> Init on label reset;

    // Emergency from any state at all
    transition wildcard "*" -> Shutdown on label emergency;
}
```

Groups and wildcards are expanded into individual transitions at realization time. `wildcard "*"` matches all states; `wildcard "Err*"` matches states whose names start with `Err`.

### Variables

Automata can declare typed state variables. Supported types are `i64`, `bool`, and enum types (see [Enums](#enums)). Each declaration starts with the `var` keyword.

```
variables {
    var count : i64 = 0;
    var ready : bool = false;
}
```

Variables are used in guard expressions and updated via effects on transitions.

Three behaviours are load-bearing and easy to get wrong:

- **`effects` are simultaneous and read the pre-state** -- non-blocking-assignment semantics. `effects { a = b; b = a; }` swaps; effects in one transition never see each other's writes.
- **A `guard` accepts exactly one comparison.** `&&`, `||` and `!` inside a guard are parsed lossily and *silently disable the transition*. Conjunctions belong in mu-calculus formulas, which support the full Boolean language.
- **Only `i64` variables bind in formula atoms.** An atom naming a `bool` variable -- or a misspelled name -- evaluates to *true* at every state, with no warning, which makes the property vacuous.

Bound every variable with a guard on the transition that increments it; an unbounded variable enumerates until it hits the state cap.

See [`docs/ctxdsl-modelling-guide.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/ctxdsl-modelling-guide.md) for the full list, each entry verified against the shipped binary, plus the checklist for trusting a hand-authored model.

### Transitions

Transitions connect states and carry one or more labels.

**Single label:**

```
transition Idle -> WaitAck on label req_assert;
```

**Multi-label** -- the transition fires when all listed labels occur simultaneously:

```
transition Idle -> Active on label req_assert, label ack_assert;
```

**Epsilon** -- an internal (silent) transition with no observable label:

```
transition Buffered -> Ready on epsilon;
```

**Guard** -- a Boolean condition on variables that must hold for the transition to be enabled:

```
transition Active -> Done on label tick
    guard count >= 3;
```

**Effects** -- variable updates that execute when the transition fires:

```
transition Active -> Active on label tick
    effects { count = count + 1; };
```

Guards and effects can be combined:

```
transition Active -> Done on label tick
    guard count >= 3
    effects { count = 0; };
```

## Composition

Compositions combine automata into larger systems. See the dedicated [Composition](Composition) page for full details.

```
composition {
    synchronous pipeline {
        members [Producer, Consumer];
    }

    asynchronous sensor_network {
        members [SensorA, SensorB, Monitor];
    }
}
```

When using parameterized automata, member references can include an index to select a specific instance:

```
composition {
    asynchronous system {
        members [Client[0], Client[1], Arbiter];
    }
}
```

This resolves `Client[0]` to `Client_0`, `Client[1]` to `Client_1`, etc.

## Mu-Calculus Formulas

Formulas are named mu-calculus expressions evaluated over a specific automaton or composition.

```
mu_formulas {
    formula safety_invariant {
        over SpiMaster;
        body = nu X. ([] X);
    }

    formula transfer_completes {
        over SpiMaster;
        body = mu X. (Complete || <> X);
    }
}
```

See the [Mu-Calculus Reference](Mu-Calculus-Reference) for the full expression language.

## Controllers

A controller block synthesizes a maximally permissive controller that restricts only controllable labels to enforce a given formula.

```
controllers {
    controller safe_handshake {
        source Handshake;
        satisfying safety_invariant;
    }
}
```

- `source` -- the automaton (or composition) to synthesize over.
- `satisfying` -- the mu-calculus formula the controller must enforce.

The synthesized controller preserves all uncontrollable transitions. If the property is unrealizable (no controller can enforce it), Mununu reports the result and can generate counterstrategy diagnostics via the CLI (`--counterexample`).

## Comments

Line comments start with `//`:

```
// This is a comment
transition Idle -> Active on label start; // inline comment
```
