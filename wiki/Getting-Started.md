# Getting Started

> **Alpha Software** — Mununu is under active development. APIs, syntax, and behavior may change. We welcome feedback and bug reports via [GitHub Issues](https://github.com/Mumunu-team/mununu/issues).

This page walks you through installing Mununu, running your first verification, and connecting the web UI.

## Prerequisites

- **Rust 1.91+** (Edition 2024). Install via [rustup](https://rustup.rs/).
- **cargo** (ships with Rust).

Verify your toolchain:

```bash
rustc --version   # must be >= 1.91
cargo --version
```

## Build

Clone the repository and build the release binary:

```bash
git clone https://github.com/Mumunu-team/mununu.git
cd mununu
cargo build --release
```

The binary is at `target/release/mununu`.

## First Example: Industrial Door Lock

The tutorial ships with a door lock controller that tracks failed unlock attempts and enters lockout after three bad codes.

Here is the CTXDSL file (abbreviated -- see `tutorial/examples/01_basic_modeling.ctxdsl` for the full version):

```
context door_lock {
    alphabet {
        label unlock;
        label lock;
        label bad_code;
        label timeout;
        label reset;
    }

    automata {
        automaton DoorLock {
            states {
                state Locked initial;
                state Unlocked;
                state Strike1;
                state Strike2;
                state Lockout;
            }

            transitions {
                transition Locked -> Unlocked on label unlock;
                transition Unlocked -> Locked on label lock;
                transition Locked -> Strike1 on label bad_code;
                transition Strike1 -> Strike2 on label bad_code;
                transition Strike2 -> Lockout on label bad_code;
                transition Lockout -> Locked on label reset;
                // ... more transitions in the full file
            }
        }
    }

    mu_formulas {
        formula safety_invariant {
            over DoorLock;
            body = nu X. ([] X);
        }

        formula lockout_reachable {
            over DoorLock;
            body = mu X. (Lockout || <> X);
        }
    }
}
```

### Evaluate a formula

Check whether the safety invariant holds over the DoorLock automaton:

```bash
mununu context eval tutorial/examples/01_basic_modeling.ctxdsl \
    --formula safety_invariant \
    --automaton DoorLock
```

### Summarize the context

Get a JSON overview of all automata, formulas, and controllers in the file:

```bash
mununu context summarize tutorial/examples/01_basic_modeling.ctxdsl
```

## Start the API Server

Mununu includes an HTTP API server (behind the `api` feature flag) that the web UI talks to:

```bash
cargo run --features api -- server
```

By default the server listens on `127.0.0.1:8080`. Use `--addr` to change it:

```bash
cargo run --features api -- server --addr 0.0.0.0:9090
```

## Connect mununu-ui

The [mununu-ui](https://github.com/Mumunu-team/mununu-ui) frontend provides interactive graph visualization, formula evaluation, and controller synthesis through your browser. Follow the setup instructions in the mununu-ui repository to point it at your running API server.

## Next Steps

- [CTXDSL Language Reference](CTXDSL-Language-Reference) -- learn the full DSL syntax for alphabets, automata, variables, guards, and effects.
- [Composition](Composition) -- combine automata using synchronous and asynchronous composition.
- [Mu-Calculus Reference](Mu-Calculus-Reference) -- write safety, liveness, and controllability properties.
