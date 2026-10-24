# TLSF Examples

TLSF (Temporal Logic Synthesis Format) is the input format for reactive synthesis — INPUTS, OUTPUTS, ASSUMPTIONS / INVARIANTS / GUARANTEES expressed in LTL. The mununu TLSF adapter parses TLSF specs, translates the LTL into a turn-based mu-calculus encoding (env / ctrl alternation), and emits a single `Signals` automaton plus a property the synthesis engine can attempt.

## `request_grant.tlsf` — Single-Channel Arbiter

A canonical reactive-synthesis sanity case:

- `INPUTS { req }` — environment input
- `OUTPUTS { grant }` — controller output
- `ASSUMPTIONS { G F req }` — environment is fair (request infinitely often)
- `GUARANTEES`:
  - `G (req -> F grant)` — every request is eventually granted
  - `G (grant -> X !grant)` — grant is at most one cycle wide

Mealy semantics (controller observes the input before producing the output for the same cycle).

## CLI

```bash
mununu context summarize examples/tlsf/request_grant.tlsf
mununu context eval examples/tlsf/request_grant.tlsf \
    --formula syntcomp_prop --automaton Signals
mununu context synth examples/tlsf/request_grant.tlsf \
    --formula syntcomp_prop --automaton Signals
```

Expected: `syntcomp_prop` holds in 8/8 reachable states; synthesis finds a realizable controller.

## API

```bash
TLSF=$(cat examples/tlsf/request_grant.tlsf)
CTXDSL=$(curl -s -X POST http://127.0.0.1:8080/api/v1/context/import \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg c "$TLSF" '{format:"auto", content:$c}')" | jq -r '.ctxdsl')
curl -s -X POST http://127.0.0.1:8080/api/v1/context/eval \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg c "$CTXDSL" \
        '{context:{name:"request_grant", content:$c},
          formula:"syntcomp_prop", automaton:"Signals"}')"
```

## UI

`.tlsf` is in `ADAPTER_EXTENSIONS` (`mununu-ui/src/api/endpoints.ts`); the editor auto-routes the file through `/import`, the summary panel exposes the synthesis property, and the synthesis workflow can run on it.
