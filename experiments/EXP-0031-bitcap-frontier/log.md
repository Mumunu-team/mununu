# EXP-0031-bitcap-frontier: where the exact engine stops deciding, per design shape, under the shipped defaults

**Author:** Mariano Cerrutti (vscorza@gmail.com)
**Date opened:** 2026-10-14
**Date closed:** 2026-10-14
**Commit baseline:** n/a (a measurement, not an A/B)
**Commit candidate:** see `manifest.json` — the binary is `d7462e5` (main after mununu#683 merged); the frontier script was uncommitted at run time, hence `git_dirty: yes`
**Container digest:** none — host run (see "Method")
**Hardware:** Intel Core i7-9750H (6c/12t) macOS, release binary; full fingerprint in `hw-fingerprint.txt`

## Motivation

See `README.md`. The one-line version: "~40 bits" was quoted everywhere as the exact engine's reach;
the code admits cones to 192 by default and retries at 1024 on the recoverability verb; nobody had
measured what actually decides, per shape, under the shipped budgets.

## Hypothesis

Pre-registered in `README.md` (four items). Item 2 — "the multiplier decides to 32 bits per operand" —
came from `scripts/profile_cases.py`'s `exact-mult` notes, which were measured with
`MUNUNU_BDD_ARENA_NODES=1<<26` and the soft bounds off. This experiment runs the DEFAULTS, so item 2
is exactly the kind of claim it exists to test.

## Method

- Instrument: `scripts/bitcap_frontier.py sweep` on `target/release/mununu` built from `de8210c`
  (mununu#682: the recoverability verbs report `engine` and `exact_abstention`). Every probe runs
  `btor2 verify-recoverability` with EVERY `MUNUNU_*` variable scrubbed from the shell and
  `RUST_LOG=error,mununu::recoverability=warn,mununu_core::adapter::btor2::symbolic_bitblast=debug`
  (the escalation line and the engine's own `kept_bits`). Wall cap 300 s per probe.
- Shapes: `fsm-timeout` (6-state FSM + W-bit timeout against a free-init limit; HOLDS), its `stuck`
  twin (absorbing ERROR; VIOLATED), `fsm-pair` (requester/granter handshake + two timers), `mult`,
  `relational`, `twocount`, `raster`, and the fixed RTL references `i2c` / `aes`
  (`crates/mununu-core/tests/fixtures/wall_classes/`). Cone formulas: fsm-timeout 2W+6, fsm-pair 3W+9,
  mult/relational 2n+1, twocount 2w+2, raster 10+vb — each cross-checked against the engine's
  `kept_bits` on every probe.
- Search: ascending seeds to the first probe the exact engine does not decide (`verdict ∈ {holds,
  violated}` with `engine ∈ {exact, exact-reach-restricted}`), bisection to adjacency (≤ 8 extra
  probes), one look past the first non-decider (the arena tier steps at 40 bits, so monotonicity is
  not assumed), and one fixed row with cone > 192 (the cap-ladder retry at 1024). Every probe kept.
- Configurations: `default`; `squaring-off` (`MUNUNU_BDD_SQUARING=0`) for raster and twocount;
  `cell-major` (`MUNUNU_BDD_VAR_ORDER=cell-major`) for relational.
- Statistical test: none — the frontier widths are determined by deterministic budgets (iteration,
  node) and reproduce; wall times are reported as the host's and are not the result.
- Host, not the `mununu-sva` image: the shapes are generated BTOR2 and the verb is pure Rust, so no
  subprocess tool is involved (the SVA rule in `CLAUDE.md` does not apply); the numbers go into
  `docs/` and the private decks, not a consumer briefing.

## Results

Every probe is in `frontier.json`; the table (with the per-row wall, engine and typed bound) is in
`docs/verifying-rtl.md` → "What decides at what width". The frontier per (shape, configuration):

| shape | config | largest deciding width (cone) | verdict · engine · wall | first non-decider (cone) | bounded by |
|---|---|---|---|---|---|
| fsm-timeout | default | W = 509 (1024 b) | HOLDS · exact · 0.5 s | W = 510 (1026 b) | `bit-cap` |
| fsm-timeout-stuck | default | W = 509 (1023 b) | VIOLATED · exact · 152 s | W = 510 (1025 b) | `bit-cap` |
| fsm-pair | default | W = 338 (1023 b) | HOLDS · exact · 0.5 s | W = 339 (1026 b) | `bit-cap` |
| mult | default | n = 9 (19 b) | VIOLATED · exact · 0.3 s | n = 10 (21 b) | `arena-overflow` (16 M arena, ≤ 40-bit tier); cube ladder VIOLATED to n = 32, `unknown` at 48 / 96 |
| relational | default | n = 511 (1023 b) | VIOLATED · exact · 0.4 s | n = 512 (1025 b) | `bit-cap` |
| relational | cell-major | n = 11 (23 b) | VIOLATED · exact-reach-restricted · 29 s | n = 12 (25 b) | wall cap 300 s (host) |
| twocount | default | w = 346 (694 b) | HOLDS · exact · 293 s | w = 347 (696 b) | wall cap 300 s (host) |
| twocount | squaring-off | w = 17 (36 b) | HOLDS · exact-reach-restricted · 5.6 s | w = 18 (38 b) | `latency` |
| raster | default | vb = 64 (74 b) | HOLDS · exact · 0.7 s | — (generator limit: the target literal past 64 bits) | — |
| raster | squaring-off | vb = 10 (20 b) | HOLDS · exact-reach-restricted · 5.2 s | vb = 11 (21 b) | `latency` |
| i2c | default | 146 kept b | HOLDS · exact · 0.2 s | — | — |
| aes | default | 37 kept b | HOLDS · exact · 0.1 s | — | — |

Cone formulas matched the engine's `kept_bits` on every probe where the build completed (the stuck
twin's formula was corrected to 2W+5 after the first pass — `clr` leaves its cone — and the shape
re-run clean). No non-monotone pair and no verdict mismatch on any shape. Load during the sweep
2.2–3.1 (1-min), no cargo workload beside it.

## Interpretation

- **Hypotheses 1, 3, 4 held; hypothesis 2 is refuted.** The control shapes decide at every width to
  the 1024-bit hard ceiling (hypothesis 1 — stronger than "to the cap": the cap ladder's retry at
  1024 carried them past 192). Squaring decides `twocount` to 694 bits inside the 300 s cap and
  `raster` to the generator's reach; without it the iteration-class `latency` bound reappears at
  38 / 21 bits (hypothesis 3). `relational` decides to 1023 bits interleaved and cliffs at 25 bits
  under `cell-major` (hypothesis 4). The multiplier leaves the exact engine at **9 bits per operand**,
  not 32: at n = 10 (21-bit cone) the bit-blast overflows the ≤ 40-bit tier's 16 M-node arena. The
  "32 bits" figure was a profiling-family measurement with `MUNUNU_BDD_ARENA_NODES=1<<26` — the wide
  tier's arena, which a 21-bit cone never gets. The predicate-cube ladder covers the gap to 32 bits.
- **The number to quote is a shape, not a width.** "40 bits" undersells control logic by a factor of
  25 and oversells a multiplier by a factor of 2. The frontier table replaces the sentence.
- **A VIOLATED costs the witness, not the fixpoint**: 0.5 s vs 152 s at 1023 bits on the same FSM.
- **Two wall-cap rows are the host's, not the engine's**: twocount at 696 bits and relational
  cell-major at 25 bits stopped at 300 s; a slower or faster host moves them; the typed bounds
  (`bit-cap`, `arena-overflow`, `latency`) do not.

## Dead-ends

- **2026-10-14 — the first sweep budgeted the seed walk.** `--max-probes 8` counted the seed walk, so
  every shape that decided everything stopped at its 8th seed (128 → 262 bits for fsm-timeout) and the
  1024-bit rows were never probed. Fixed: the walk is unbudgeted (a deciding probe is fast by
  definition), the budget applies to the bisection and the look-past.
- **2026-10-14 — raster's 192-bit row is out of the generator's reach.** The probe at vb = 183 exited
  1 before the engine ran: the TARGET `vcount == 2^183-1` is a predicate literal the atom parser folds
  through a fixed-width integer (`bad decimal literal … number too large to fit in target type`). Not
  an engine bound; reported as `unrunnable`, excluded from the frontier, and the table says so. The
  BTOR2 `constd` itself parses at any width (mununu#650), which is why twocount — target `done == 1` —
  reaches the ceiling.
- **2026-10-14 — the look-past probe re-ran an already-probed seed** (mult 16, raster 12). Fixed to skip
  probed widths.

## Followups

- The multiplier's frontier is set by the ≤ 40-bit arena tier (16 M nodes); the wide tier (67 M) is
  what the profiling family measured. Whether the tier step should move, or the arena should scale
  with the cone's *shape* rather than its bit count, is an engine-performance-roadmap question, not a
  documentation one.
- A predicate-literal type wider than the atom parser's fixed-width integer would let raster (and any
  target against a wide constant) run past 128 bits.

## Artifacts

- `frontier.json` (sha256 in `manifest.json`)
- `hw-fingerprint.txt` (sha256 in `manifest.json`)
