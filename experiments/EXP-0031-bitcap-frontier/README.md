# EXP-0031-bitcap-frontier: where the exact engine stops deciding, per design shape, under the shipped defaults

**One-line summary.** Bisected the exact engine's deciding frontier on nine design shapes under the
shipped defaults (every `MUNUNU_*` scrubbed, release binary, 300 s wall cap per probe) and recorded,
per shape, the largest width it decides and the first it does not, with the typed abstention. Headline:
**width is not what walls the exact engine; shape is** — FSM-plus-timer control logic decides at every
width up to the 1024-bit ceiling, while a multiplier leaves the exact engine at 9 bits per operand
under the default 16 M-node arena (the predicate-cube ladder still decides it to 32). The frontier
table is in [`docs/verifying-rtl.md`](../../docs/verifying-rtl.md) ("What decides at what width").

## Motivation

Every deck, doc and reply quoted "~40 bits" as the exact engine's reach. The code says otherwise:
`MAX_BITBLAST_BITS = 40` is the admission floor, `default_cap_for_cone` raises the cap to fit the
cone up to `AUTO_CAP_CEILING = 192`, and the recoverability verb retries a bit-cap abstention once at
the 1024-bit ceiling (`crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs`). What actually
bounds a run is a budget the shape hits — the arena, the iteration budget, the latency bound — and
until mununu#682 the recoverability verbs did not say which engine decided or why the exact one
stopped, so nobody had measured the frontier per shape. The Emtech meeting (week of 2026-10-13)
made the question concrete: FPGA engineers will ask for the real number.

## Hypothesis (pre-registered in the plan, 2026-10-14, before the sweep)

1. The two control-logic shapes (`fsm-timeout`: a 6-state FSM with a W-bit timeout against a
   free-init limit; `fsm-pair`: two such machines coupled by a handshake) decide at **every** width
   to the cap — their BDDs (increment, equality) are linear in W.
2. The multiplier decides VIOLATED to 32 bits per operand and then abstains on the node budget
   (`arena-overflow`). *(From the profiling family's notes.)*
3. `raster` and `twocount` decide to the cap through the squaring rescue; their iteration frontier
   reappears only with `MUNUNU_BDD_SQUARING=0`.
4. `relational` decides to the cap under the default interleaved order and shows the representation
   cliff under `cell-major`.

## Headline result

See `log.md` (results, interpretation, the refuted hypothesis) and `frontier.json` (every probe).

Tests: `make frontier-smoke` (the known points, asserted) green on the release binary at the
recorded commit.

## How to replay

```bash
cargo build --release -p mununu-cli
scripts/bitcap_frontier.py sweep --fresh            # every shape × its configurations (≈ 1½ h on the dev host)
scripts/bitcap_frontier.py table                    # the markdown table
```

`make replay` does not apply: the measurement is this script, not a Criterion bench. Full
provenance: `manifest.json` (schema v1; `criterion_archive_sha256` is empty by design), hardware:
`hw-fingerprint.txt`, raw output: `frontier.json`, lab notebook: `log.md`.

## Files in this archive

- `README.md` — this file.
- `manifest.json` — provenance: commit SHA, branch, dirty flag, host, binary sha256, timestamps.
- `command.txt` — exact replay command.
- `hw-fingerprint.txt` — output of `scripts/capture_hw.sh` at record time.
- `frontier.json` — every probe (verdict, engine, exact abstention, iterations, peak nodes, work, wall, kept bits, rss), per (shape, configuration), plus the sweep's provenance block.
- `log.md` — dated lab-notebook entry.

## Status

`closed`
