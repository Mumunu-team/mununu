# The executable ledger

> Source of truth: [`scripts/ledger_check.py`](../../scripts/ledger_check.py) · [`adapter::row_facts`](../../crates/mununu-core/src/adapter/row_facts.rs) · [`ModelDiagnostics::cone_hashes`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: CLI (`make ledger-check`, in the `mununu-sva` image)

N0.1 (2026-11-03). The benchmark-property ledger used to be prose nobody updated and no gate
could diff. This directory is the measured one: `verdicts.csv` holds, per (design, row), the
**cone hash** of what decides the row, the verdict, the decider, the seconds; `make ledger-check`
re-runs only the rows whose cone changed since they were measured, whose decider was retired, or
that are new — a lift per design, no engine for an unchanged row.

## Files

| file | what |
|---|---|
| `designs.tsv` | `design<TAB>cwd<TAB>command`, one line per design; the command is an `sv verify-auto` invocation (the vendored `run.sh` of an IP), run with `mununu` on `PATH`. |
| `verdicts.csv` | the measured rows: `design,row,property,label,cone_hash,outcome,decided_by,bottom_reason,elapsed_ms,measured_at,engine_rev`. `row` is the label when the property has one, else the positional name. Written by `make ledger-check LEDGER_ARGS=--write`; never by hand. |

## How a check runs

1. **Facts** — every design runs once with `MUNUNU_VERIFY_AUTO_ONLY_ROWS=-` (no row runs) and
   `MUNUNU_VERIFY_AUTO_ROW_FACTS`: the lift, the extraction and every row's cone hash, no engine.
2. **Select** — a row is re-run when it is NEW, when its hash CHANGED, when its recorded decider
   is in `--retired` (the N2/N3 removals: `--retired spacer`), or always under `--all` (breadth —
   the unknown rows are otherwise left alone, so a check costs minutes, not the hour the ⊥ rows
   would take to re-abstain).
3. **Run** — the selected rows, by name or label, under `MUNUNU_PROPERTY_BUDGET_MS` of 3× the
   recorded time (10 s floor, 60 s cap); seconds come from `MUNUNU_PROPERTY_TIMING`'s lines.
4. **Compare** — `CONTRADICTION` (a definite verdict became the other one) and `REGRESSION`
   (definite → unknown) exit 1 and are not written over; `IMPROVED` (unknown → definite), `SAME`,
   `NEW` and `RETIRED` rows are recorded with `--write`. A design that produces no facts is an
   error (exit 2), never a silent zero.

The cone hash is FNV-1a over the formula and the text of the BTOR2 slice the property depends
on (data flow plus constraint and fairness coupling, NIDs preserved). It moves whenever what
decides the row moves, and also when a change elsewhere renumbers the design — a false miss the
check pays with one re-run, never a false hit.

## What it is for

- **N1 (slang-only lift)**: the gate "verdicts identical on the 187" becomes `make ledger-check`
  after the lift change — every row's cone hash moves (a new netlist), every row re-runs under its
  own budget, and the diff is the finding list.
- **N2 / N3 (engine removals)**: `--retired spacer` re-runs only the rows the removed member
  decided.
- **Any engine change**: an unchanged cone with a moved verdict is a contradiction the check
  refuses to record.
