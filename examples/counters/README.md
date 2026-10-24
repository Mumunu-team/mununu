# Counters Example (Main + Sidecar Pattern)

Two files demonstrating the main-context + sidecar pattern:

- `counters.ctxdsl` — main context. Defines the alphabet (`tick`) and the `Counter` automaton (`Idle -> Active`).
- `counters_properties.ctxdsl` — sidecar. Declares formulas (`reachability`, `safety_idle`) and a controller over `Counter` without redefining the automaton itself.

The sidecar references `Counter` by name; the main file must supply it. Mununu merges them at load time when you pass the sidecar via `--sidecar` (or copies them together with `mununu context merge`).

## CLI

```bash
# Verify reachability on the merged context
mununu context eval examples/counters/counters.ctxdsl \
    --sidecar examples/counters/counters_properties.ctxdsl \
    --formula reachability --automaton Counter

# Synthesize the controller declared in the sidecar
mununu context synth examples/counters/counters.ctxdsl \
    --sidecar examples/counters/counters_properties.ctxdsl \
    --formula reachability --automaton Counter
```

You can also pre-merge the files into a build directory:

```bash
mununu context merge \
    examples/counters/counters.ctxdsl \
    examples/counters/counters_properties.ctxdsl \
    --output build/counters
```

## API

The HTTP API takes the main context plus sidecars in the same request:

```bash
curl -s -X POST http://127.0.0.1:8080/api/v1/context/eval \
    -H 'Content-Type: application/json' \
    -d "$(jq -n \
        --arg main "$(cat examples/counters/counters.ctxdsl)" \
        --arg side "$(cat examples/counters/counters_properties.ctxdsl)" \
        '{context: {name:"counters", content:$main},
          sidecars: [{name:"counters_properties", content:$side}],
          formula: "reachability", automaton: "Counter"}')"
```

## UI

In `mununu-ui`, open `counters.ctxdsl` in the editor and add `counters_properties.ctxdsl` via the sidecar list. The summary panel shows both `Counter` (from the main file) and the merged formulas / controller from the sidecar.
