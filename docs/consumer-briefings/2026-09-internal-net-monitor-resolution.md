# Consumer briefing — 2026-09 SVA properties over internal nets now decide

> **Audience:** monono (pins verdicts in its formal lane), ROSF, anyone running `mununu sv verify-auto` on SVA with implication properties.
>
> **Related:** [mununu#503](https://github.com/Mumunu-team/mununu/issues/503). Builds on #527 and #528.
>
> **TL;DR:** a `|->` property whose antecedent or consequent is an **internal combinational wire** (`assign trigger_active = …`) was returning `unknown` — not because the property was hard, but because the safety-rescue monitor could not reference that net. Fixed. **Verdicts change in one direction only: `unknown` → `holds` or `violated`.** No definite verdict flips.

## What was wrong

When the predicate-cube abstraction leaves an AG-safety property ⊥, mununu escalates it to the
**safety-rescue lane**: it compiles the property into a `bad`-monitor and runs the *concrete*
reachability portfolio (native BMC, k-induction, SPACER, btormc, pono) on the real design.

That monitor could resolve a leaf that named a **state cell**, an **output port**, or a **primary
input**. It could not resolve a **named internal net** — so it refused to compile, the rescue
declined, and the ⊥ stood.

Internal nets are extremely common in real SVA. `sysrst_ctrl_detect` is typical:

```systemverilog
assign trigger_active = (trigger_i == 1'b0);
...
`ASSERT(DetectedOut_A, state_q == StableSt && trigger_active && cfg_enable_i |-> event_detected_o)
```

Every property over `trigger_active` was undecidable for this reason alone.

## What changed

Yosys names an internal net with a no-op alias line — `<nid> uext <sort> <src> 0 <name>` — so the
symbol sits on an ordinary combinational node carrying both nid and sort. The monitor now resolves
those, **last** in the chain: a name that is also a state cell, output port, or input still binds
to that first, so no existing binding is re-pointed.

## Direction of change

- **`unknown` → `holds`**, or **`unknown` → `violated`**.
- **No `holds` becomes `violated`, and no `violated` becomes `holds`.**

`violated` is in that list deliberately: if one of these properties was masking a real failure, you
will now see it. That is the point.

## Measured

On `sysrst_ctrl_detect` with the config timers concretized:

| property | before | after |
|---|---|---|
| `sva_10` (`DetectedOut_A`) | `unknown` (32 cells) | **`holds`** |
| `sva_11` | `unknown` (16 cells) | **`holds`** |

Both had been undecided since mid-July.

## Scope — what is NOT measured

**I have not measured how many properties across a real corpus this affects.** The rule is
mechanical — any AG-safety property the cube leaves ⊥ whose leaves are internal nets can now reach
the rescue lane — but the *count* on your designs is unknown to me. If you pin verdicts, expect
some `unknown` rows to move; each one is a property that was never actually being checked.

**There is no independent verdict-level cross-check for `sva_10`/`sva_11`.** I ran the
exact-symbolic engine as a differential and it `skipped` both (it abstains on this shape). The
verdicts do come from the concrete reachability portfolio rather than the abstraction that was
returning ⊥, so they are not self-confirming — but I would rather say this plainly than imply a
corroboration I did not obtain.

The mechanism is pinned by a **negative control**: a fixture where `hot` aliases `a`, so
`AG(!(a==1) || !(hot==1))` is falsified by `a = 1`, and the monitor must report `violated`. A
resolver that mis-bound the leaf would most likely *miss* that and report `holds` — which no
verdict comparison would catch. A positive twin rules out the opposite error.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod) | verdict semantics on the rescue lane | **Yes** |
| mununu `Dockerfile.dev` | binary bump | **Yes** |
| mununu `Dockerfile.sva` | binary bump; the e2e runs here | **Yes** |
| mununu `Dockerfile.extract`, `.extract-*` | no rescue path | No |
| rosf | consumes verdicts | **Yes if it pins expected verdicts**, else No |
| monono Docker | pins verdicts in `verify.sh` | **Yes** |
| mununu-ui | no type change | No |

## Verification

```bash
cargo test -p mununu-core --lib -- internal_net_leaf
```

The negative and positive controls above. End-to-end in `mununu-sva`:
`e2e_sysrst_config_concretization_flips_timer_relationals` now passes.

## Still undecided, and why — no longer a mystery

Properties that remain ⊥ on this design now say what blocks them
(`safety-rescue-declined`, added in #528):

- `sva_12` — `AG(a → AX b)`, the `|=>` shape; the rescue lane has no reducer for it yet.
- `sva_13/14/15` — relational leaves (`cnt_q >= cnt_q__past`) the compound monitor cannot encode.

Both are shape gaps, not resource limits: a bigger budget will not move them.
