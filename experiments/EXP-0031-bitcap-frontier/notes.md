# Free-form observations for EXP-0031-bitcap-frontier

- `kept_bits` is printed only when the bit-blast build SUCCEEDS; an `arena-overflow` probe has no line,
  so the table shows the generator's formula for those rows and the engine's count where both exist.
- tracing colours field names (`\x1b[3mkept_bits\x1b[0m\x1b[2m=\x1b[0m65`): the stderr regexes run on
  ANSI-stripped text (`profile_cases.ANSI_RE`), or `kept_bits=` never matches.
- A VIOLATED on the `stuck` FSM twin costs 5–10 s where its HOLDS twin costs 0.2 s: the shortest-path
  witness extraction, not the fixpoint.
- i2c's cone is 146 kept bits at bit-level cone of influence (the "173" quoted earlier was the
  signal-level count).
- The multiplier at n = 16 (33-bit cone) overflows the ≤ 40-bit tier's arena at 2.26 M of 2.10 M live
  nodes — the build phase, before any fixpoint; at n = 32 the WIDE tier overflows at 55.7 M of 53.7 M.
