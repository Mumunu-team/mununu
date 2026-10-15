# btor2tools example suite — provenance

These BTOR2 safety benchmarks are vendored **byte-for-byte** from the upstream
[Boolector/btor2tools](https://github.com/Boolector/btor2tools) `examples/btorsim/`
directory, at commit `d33c73ff1d173f1bfac8ba6b1c6d68ba62c55f8e`. They are real,
hand-independent model-checking benchmarks (some, e.g. `ponylink-*`, are extracted
from published hardware) used here as an **independent reachability oracle**: each
carries `bad` properties whose reachability `mununu`'s exact engine
(`exact_bad_reachable`) is cross-checked against **btormc** in
`hwmcc_style_coverage_study` (differential_oracle_e2e.rs). Untouched — only read,
never rewritten (claims-integrity: models-from-source).

## License

btor2tools is **MIT-licensed**:

> Copyright (c) 2012-2018 Armin Biere.
> Copyright (c) 2013-2018 Mathias Preiner.
> Copyright (c) 2015-2018 Aina Niemetz.
>
> Permission is hereby granted, free of charge, to any person obtaining a copy of
> this software and associated documentation files (the "Software"), to deal in
> the Software without restriction, including without limitation the rights to
> use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
> of the Software, and to permit persons to whom the Software is furnished to do
> so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all
> copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
> IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS
> FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
> COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
> IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
> CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

## The suite (why this spread)

The set is chosen to exercise every branch of the coverage study — what our exact
engine decides, what it soundly refuses, and where it hits the bit cap:

| File | bits | `bad` reachable? | mununu exact engine |
|---|---|---|---|
| `count2.btor2` | 3-bit counter | **yes** (reaches `0b111`) | decides (SAT) |
| `count4.btor2` | 4-bit counter | **yes** | decides (SAT) |
| `recount4.btor2` | 4-bit counter + enable/reset | **yes** | decides (SAT) |
| `twocount2.btor2` | two 2-bit counters | reachable | decides |
| `factorial4even.btor2` | 4-bit, uses `mul` | reachable | decides (exercises the `Mul` bit-blaster) |
| `twocount32.btor2` | 32-bit | — | decides / near cap |
| `noninitstate.btor2` | has a `constraint` | — | **refused** (constraint not modelled — soundness guard) |
| `twocount2c.btor2` | has a `constraint` | — | **refused** (constraint guard) |
| `ponylink-slaveTXlen-sat.btor2` | 228-bit, 320 states | **yes** (`-sat`) | **Skipped** (228 bits is over the exact engine's 192-bit auto-cap) — btormc decides |

The `-sat` / `-unsat` filename suffix is the upstream competition convention for the
known verdict (`sat` = `bad` reachable / unsafe; `unsat` = safe). The study asserts
every verdict mununu's engine emits AGREES with btormc; it never fails on a
`Skipped`/refused case (that is honest coverage, not a wrong answer).
