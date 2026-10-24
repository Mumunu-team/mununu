# AIGER Examples

AIGER is the standard format for sequential circuits: inputs, latches, AND gates, and `bad` outputs (safety obligations). The mununu AIGER adapter parses ASCII AAG files, encodes each latch as a state bit, and turns each `bad` output into a safety formula.

## `alarm.aag` — Sticky Alarm Latch

A 6-line circuit with one input (`sensor`), one latch (`alarm`), and one bad output (`alarm_on`).

```
aag 3 1 1 0 1 1   # max-var=3, 1 input, 1 latch, 0 outputs, 1 AND gate, 1 bad
2                 # input literal: sensor (lit 2 = !1)
4 7               # latch: current=4, next=7 (= AND(3,5) = AND(sensor, !alarm) ... actually: next = OR via NAND)
4                 # bad output: literal 4 = alarm
6 5 3             # AND gate: 6 = AND(5, 3) = AND(!alarm, sensor)
i0 sensor
l0 alarm
b0 alarm_on
```

In words: the latch `alarm` becomes 1 once `sensor` fires and stays 1 forever (sticky). The bad output `alarm_on` is true when `alarm = 1`. The safety property `safety_alarm_on` says "the bad output is always false" — i.e., the alarm never turns on.

Since `sensor` is uncontrollable, the environment can always force the alarm: the safety property is **unrealizable**, and synthesis confirms that.

## CLI

```bash
mununu context summarize examples/aiger/alarm.aag
mununu context eval examples/aiger/alarm.aag \
    --formula safety_alarm_on --automaton Circuit
mununu context synth examples/aiger/alarm.aag \
    --formula safety_alarm_on --automaton Circuit
```

Expected from `eval`: 0/2 states satisfy (the bad state is reachable from initial). Expected from `synth`: unrealizable — no controller can prevent the latch from sticking.

## API

```bash
AAG=$(cat examples/aiger/alarm.aag)
CTXDSL=$(curl -s -X POST http://127.0.0.1:8080/api/v1/context/import \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg c "$AAG" '{format:"auto", content:$c}')" | jq -r '.ctxdsl')
curl -s -X POST http://127.0.0.1:8080/api/v1/context/eval \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg c "$CTXDSL" \
        '{context:{name:"alarm", content:$c},
          formula:"safety_alarm_on", automaton:"Circuit"}')"
```

## UI

`.aag` and `.aig` are in `ADAPTER_EXTENSIONS` (`mununu-ui/src/api/endpoints.ts`); the editor auto-routes the file through `/import` and the verification / synthesis panels can run on it.
