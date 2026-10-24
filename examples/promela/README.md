# Promela Examples

Promela is the input language for the SPIN model checker — concurrent processes with shared variables and channel communication. The mununu Promela adapter parses Promela source, builds CFG-based process automata + variable automata, and composes them asynchronously.

## `mutex_simple.pml` — Peterson's Mutual Exclusion

Two processes (`P0`, `P1`) compete for a critical section guarded by Peterson's algorithm with a turn variable and per-process flags. The `ltl mutex` block declares the safety property `[] !(cs0 && cs1)`: both critical-section indicators are never simultaneously true.

The adapter emits one automaton per process (`P0_cfg`, `P1_cfg`), one per byte / bool variable (`Var_turn`, `Var_flag0`, `Var_flag1`, `Var_cs0`, `Var_cs1`), and an asynchronous composition `System` that holds them all together.

## CLI

```bash
# Auto-detect from the .pml extension
mununu context summarize examples/promela/mutex_simple.pml
mununu context eval examples/promela/mutex_simple.pml \
    --formula mutex --automaton System
```

Expected: `mutex` holds in 2112/2112 states (the full reachable product); initial state satisfies. Peterson's algorithm provably enforces mutual exclusion.

## API

The Promela source must first be translated to CTXDSL via `/api/v1/context/import`, then verified:

```bash
PML=$(cat examples/promela/mutex_simple.pml)
CTXDSL=$(curl -s -X POST http://127.0.0.1:8080/api/v1/context/import \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg c "$PML" '{format:"auto", content:$c}')" | jq -r '.ctxdsl')
curl -s -X POST http://127.0.0.1:8080/api/v1/context/eval \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg c "$CTXDSL" \
        '{context:{name:"mutex_simple", content:$c},
          formula:"mutex", automaton:"System"}')"
```

## UI

The `.pml` extension is in `ADAPTER_EXTENSIONS` (`mununu-ui/src/api/endpoints.ts`); dropping the file in the editor auto-routes it through the import endpoint, exposes `mutex` in the formula list, and lets the verification panel run it.
