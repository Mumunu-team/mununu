# `f1_array_read_capture_timing` — does an array read's timing survive the lift? (mununu#545, roadmap F1)

> Source of truth: [`line_table.sv`](line_table.sv), [`line_table_sva.sv`](line_table_sva.sv), [`validate.sh`](validate.sh) — surface: CLI (`mununu sv verify-auto … --source <top>_sva.sv --top <top> --expect …`); the same request over `POST /api/v1/sv/verify-auto`.

> **Synthetic fixture (hand-written, NOT vendored RTL).** It reproduces the *shape* a consumer
> reported in mununu#545, not their code: a table loaded by a controller, read continuously at a
> free index, sampled into a register once per strobe. Values are irrelevant to every property
> here; timing is everything.

## The question

mununu#545 was closed on 2026-10-08 as *not planned*, with one condition for reopening it: build
"a minimal in-repo fixture (an unconditional continuous read at a free index feeding a
strobe-gated capture register, the two `|=>` properties, and a twin that breaks the capture
timing)" and measure it "when the array-νμ track is next active". That track became active on
2026-11-07, when native BMC / k-induction learned to encode memories (N5, #870). This directory
is that fixture.

The three accesses, kept distinct on purpose (the issue's second correction merged two of them
and drew the wrong conclusion):

| # | access | in `line_table.sv` | character |
|---|---|---|---|
| 1 | the array **write** | `if (we) mem[waddr] <= wdata;` | conditional, at its own index |
| 2 | the array **read** | `assign mem_rd = mem[line];` | unconditional, continuous, at a **free** index that may advance every cycle |
| 3 | the **capture** | `if (line_stb) ent_q <= mem_rd;` | a plain register downstream of the read; not a memory access |

The two properties, bound in from `line_table_sva.sv`:

```systemverilog
a_enable_holds_between_strobes:  (enable && !line_stb) |=> enable
a_disable_holds_between_strobes: (!enable && !line_stb) |=> !enable
```

They say *when* the captured entry may change, never *which* value it holds. The twin,
`line_table_twin.sv`, differs by one line: the capture ignores the strobe. Both properties must be
VIOLATED on it; an abstraction under which they hold on the twin has removed the very timing it
claims to check, which is what `--cutpoint` on the read did for the consumer.

## What each outcome means

| outcome | reading | next step |
|---|---|---|
| design: both **hold**; twin: both **violated** | the array-aware engines decide the shape as written; #545 was a **routing** gap, closed by N5 | record it on #545 and close the thread; the consumer's blocks need no new mechanism |
| design: ⊥ on either property | k-induction over the memory does not close (the read's value is unconstrained across the strobe); the shape needs a **value-only havoc at the read** that keeps its position | reopen #545 with this fixture as the reproducer; that is design work, and the fixture is its gate |
| twin: ⊥ with `bottom_reason: unestablished-initial-state` | the table had no established initial state, so the tool withheld the definite VIOLATED (a free cycle-0 state licenses HOLDS, never VIOLATED); the twin's fault is not reached from a known state | establish the table's initial state in the fixture with an `initial` clear (done in version 3) and re-run; not a finding against the lift |
| any run with `cone_array_states = 0` on these rows (the row-facts column, mununu#900) | the table was **registerized** by the lift (a reset that writes every entry does this) and the run never exercised the array path | the run says nothing about #545, whatever its verdicts; fix the fixture, not the conclusion |
| twin: anything else but violated | the lift has lost the capture's timing | a soundness finding against the lift, not against #545; file it with the BTOR2 dump |
| any engine abstaining with a reason other than ⊥ | read `diagnostics` and the row's `portfolio.refused` before concluding anything | per the Failure Paths policy |

`engine:` whichever portfolio member decides; on a table this small the first run was decided by
the `explicit` predicate-cube engine, not by native BMC / k-induction. The exact-symbolic ROBDD
refuses the array operator (`Read` is outside its bit-blast set, as the issue measured three
ways); native BMC / k-induction under `QF_AUFBV` (N5) is the member that answers when the cube
engines cannot. The properties are plain safety, so no must-side `∀`-array query is constructed.

## Measured so far

**First run (2026-11-08, second machine, `mununu 0.5.10 (24bb5d7)`, fixture at `44125c7`):** the
design's two rows **hold** (`decided_by: explicit`, 2/2 decided, `--expect` met). The twin did not
elaborate as shipped (the design's `bind` line sat in `line_table_sva.sv`, which the twin run also
compiles; fixed, one bind per file) and, with that worked around, its two rows were **⊥** with
`bottom_reason: unestablished-initial-state`: the table `mem` was never reset, so the tool withheld
the definite VIOLATED. Both variants were then given a reset clear of the table (version 2, `a4b7ba2d`).

**Run 2 (2026-11-08, second machine, `mununu 0.5.10 (94eebfe)` = main `8e7fdc9b`, fixture `a4b7ba2d`):**
`validate.sh` exits 0 — design 2/2 **hold**, twin 2/2 **violated** with finite traces (4 and 3 steps,
model-only, not replayed under Verilator) — **but every row is decided by `exact-symbolic`, and the
row-facts column reads `cone_array_states = 0 []` on all four rows** (run 1's unreset fixture read
`1 ['mem']`): the async-reset clear made yosys turn the table into four registers (0 array sorts in
the BTOR2 dumps, five states), so run 2 measured a registerized table and says nothing about #545's
shape. Version 3 (this one) replaces the reset clear by an `initial` clear, which keeps the array
and establishes its cycle-0 value. **Run 3 is valid only if `cone_array_states` reads `1 ['mem']` on
all four rows**; its verdicts then answer the table above.

## Reproduce

Inside the `mununu-sva` image, with the release binary on `PATH`:

```bash
bash examples/verify/f1_array_read_capture_timing/validate.sh
```

Exit 0 means the first row of the table above. The JSON reports land in `logs/`.

## See also

- mununu#545, the consumer's ask and its two corrections, which fixed the shape used here.
- `docs/verifying-rtl.md` § "What decides at what width", for why the exact engine is not the
  decider on an array design.
