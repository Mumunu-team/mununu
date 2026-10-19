# Consumer briefing — rIC3 joins the reach portfolio; btormc and pono read mununu's designs again (2026-10)

> Source of truth: [`decide_via_ric3`](../../crates/mununu-core/src/adapter/ric3/mod.rs) + [`emit_btor2_for_subprocess`](../../crates/mununu-core/src/adapter/btor2/emit.rs) — surface: (CLI+API) — `sv verify-auto` / `POST /api/v1/sv/verify-auto`, `btor2 verify`

**Audience:** rosf, monono, mununu-ui and anyone pinning `sv verify-auto` verdicts or `decided_by`. Plan 4, PR D2 (mununu#736, the bit-level IC3 lever of #732's residue); follows #735 and #737.

## TL;DR

- **New verdicts.** The reach portfolio gains **rIC3** (`rIC3 -e ic3`, bit-level IC3/PDR, shipped in `mununu-sva`), a subprocess member trusted like btormc / pono. It proves the inductive-invariant safety class the portfolio could not close: OpenTitan `edn`'s five and `csrng`'s four duplicated-counter fault-flag chains (`$rose(prim_count.err_o) |-> ##[0:30] alert_p`) move from `unknown` (`budget-expired`) to **`holds`** (`decided_by: reach-rescue`, engine `ric3`): edn's own 52 assertions go 39 / 8 / 5 / 0 → **44 / 8 / 0 / 0** (holds / violated / unknown / unsupported) in 556 s, csrng 30 / 0 / 4 / 3 → **34 / 0 / 0 / 3** in 442 s.
- **btormc and pono work again on `sv verify-auto` designs.** The BTOR2 handed to a subprocess member is now emitted with every constant before every state (`emit_btor2_for_subprocess`, a renumbering). Since the automatic reset-value `init` landed (mununu#578) both members had rejected every mununu-prepared design (`state id must be greater than id of second operand`, btor2tools' rule that an `init` value precede its state) and abstained in ~40 ms; nothing noticed because mununu's own parser is lenient. Rows those members now decide first will show `decided_by` engine labels `btormc` / `pono` in the rescue notes.
- **A new engine label** `ric3` in `portfolio-rescue` notes (`engine(s): ric3`), the README engine table and `docs/design/engine-routing.md`; a new binary-path override `MUNUNU_RIC3_PATH` (and `MUNUNU_BTORMC_PATH`, which existed and was undocumented).
- No JSON shape change. Rows move `unknown` → `holds` / `violated` only.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `--expect NAME=unknown` pins on duplicated-counter fault-flag rows break where they now decide — the pin doing its job; `decided_by` may read `btormc` / `pono` / `ric3` through the rescue | rerun; move pins |
| **monono** | same; no monono row is of this shape on the lane as of 5040d46 (recorded in its Ask 44) | rerun the lane at the next pin |
| **mununu-ui** | a new engine label in notes | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` | yes | the member and the emission live in the binary; the image already ships `rIC3` (now asserted at build time) |
| `mununu-sva-pono` | yes | same |
| `mununu-ui` | no | JSON unchanged |

## Measured

`mununu-sva`, shipped defaults, this branch against the post-#737 tree:

| run | before | after |
|---|---|---|
| OpenTitan `edn`, its own 52 assertions (`run.sh`) | 39 holds / 8 violated / 5 ⊥ / 0 unsupported, 966 s | **44 / 8 / 0 / 0**, 556 s — the five `prim_count` / FIFO-pointer fault-flag chains HOLD through the reach rescue (rIC3, 2.5–4.5 s a row); btormc is the first decider on 11 rows and joint on 13 more (it had not parsed a mununu design since the reset `init` was appended) |
| OpenTitan `csrng` (cipher black-boxed) | 30 / 0 / 4 / 3, 469 s | **34 / 0 / 0 / 3**, 442 s — the four fault-flag chains HOLD by rIC3 in 42–61 s a row under portfolio load (at the edge of the 60-s rescue slice: `MUNUNU_RESCUE_SLICE_MS` is the knob there) |
| `edn` + the 15 system properties (`e2e_opentitan_ip_edn_full`) | 51 / 8 / 8 / 0 | **56 / 8 / 3 / 0**, 1 127 s — the five fault-flag rows HOLD by `reach-rescue` (10.2–10.3 s a row: rIC3 answers in ~2 s, the row then waits for z3 SPACER's uncancellable in-process 10-s query — the wall floor of every rescued row; `MUNUNU_SELF_EXE` makes SPACER a cancellable child); the three `AG EF` recoverability rows stay ⊥ (405-bit cone, research); 34 rows decided by the rescue, 16 by the cube, 14 constant |
| `e2e_opentitan_sva_census` (134 rows) | 84 / 11 / 31 skipped / 8 unsupported / 0 ⊥ | **identical verdicts**, 1 882 s (run beside a concurrent HWMCC adjudication) |
| the three subprocess members on the REACH / SAFE fixtures (`decide_via_{btormc,pono,ric3}_emits_and_decides`) | btormc / pono: refused every mununu emission | **all three decide** |
| `portfolio_agrees_across_engines_in_sva` | — | PASS with rIC3 in the portfolio |
| HWMCC'20 bv adjudication (`hwmcc_adjudication_over_user_dir`, the 67 instances under 60 KB) | — | 42 of the 67 instances adjudicated before the harness died on `vis_arrays_bufferAlloc` (SIGSEGV in-process even with the isolated SPACER child — mununu#738, not the new member): 5 reachable / 20 unreachable / 17 unknown, **no contradiction**; pono decides HWMCC instances again (`unreach_by=["exact", "spacer", "pono"]`), rIC3 is cancelled by faster members there and adds no first verdict on this subset |

No contradiction alarm anywhere; every rIC3 verdict agrees with the member it shares a row with (btormc / native on the `EdnFatalAlertNoRsp_A` violations and the bus-stability proofs).

## Not covered here

- The three `AG EF` recoverability rows of edn (research).
- rIC3's `portfolio` mode (eleven engines) — the member runs the single `ic3` engine so the reach portfolio's own parallelism stays bounded.
