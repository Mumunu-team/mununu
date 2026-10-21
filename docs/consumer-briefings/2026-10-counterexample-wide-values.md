# Consumer briefing — counterexample cells wider than 64 bits are listed; `CexCellView` gains `hex` and `bits`, `value` becomes nullable (2026-10)

> Source of truth: [`CexValue`](../../crates/mununu-core/src/adapter/btor2/cex_value.rs) + [`CexCellView`](../../crates/mununu-core/src/api/models.rs) — surface: (CLI+API) — `sv verify-auto` (`--json` and the human trace) / `POST /api/v1/sv/verify-auto` (`properties[].counterexample`), `btor2 verify --witness`

**Audience:** rosf, monono, mununu-ui and anyone reading `counterexample.prefix` / `cycle` / `inputs`. PR D of the 2026-10-20 open-issue round (mununu#723); follows PRs A (#745), C (#746) and B. **This one changes a wire shape** — the migration is one line, below.

## TL;DR

- **Every named cell appears in every frame of a counterexample, at its full width.** The native BMC witness extractor did `as_u64()` inside a `filter_map`, so a cell whose value did not fit 64 bits was dropped from the frame without a trace: on OpenTitan `edn`'s violated `EdnFatalAlertNoRsp_A` the trace listed `alert_rx_i`, `clk_i`, `csrng_cmd_i`, `edn_i` — and not `tl_i` (109 bits), the bus write that raises the fatal alert. The exact engine's lasso narrowed its `u128` values to `u64` silently, and the Boolector witness dropped wide cells the same way. One value type (`CexValue`, any width) now flows from the three witness producers to the report.
- **Wire shape (`CexCellView`, every `counterexample.prefix[][]` / `cycle[][]` / `inputs[][]` cell):**

  | field | before | after |
  |---|---|---|
  | `register` | string | unchanged |
  | `value` | `integer` (uint64), required | **`integer \| null`** — unchanged for every cell that fits 64 bits (exactly the cells that were listed before), `null` for a wider value |
  | `hex` | — | **new, always present**: `"0x…"`, zero-padded to the cell's width |
  | `bits` | — | **new, always present**: the cell's width |

  **Migration (one line):** read `value` when it is a number; when it is `null`, read `hex`. A consumer that only ever saw narrow cells sees the same numbers it saw before, plus two extra fields per cell. Pinned in `docs/api-schemas/sv-verify-auto-response.schema.json` (drift test updated).
- **CLI.** The human trace prints a wide value as `name=0x…` (decimal for a narrow one, as before). `btor2 verify --witness` keeps `name: <number>` for a cell that fits and prints `name: "0x…"` (a string) for a wider one instead of omitting it — that JSON is not schema-pinned; a consumer parsing it as numbers only should accept the string form.
- **The exact engine's witness** now refuses (no witness, a warning) when any cell — an input too — is wider than 128 bits; before, an input wider than 128 bits reached a `1u128 << b` shift overflow that the unwind guard hid as a lost witness. A full-width exact witness beyond 128 bits is a follow-up (noted below).
- **Verilator replay** keeps driving `u64` values; a witness with a wider cell is refused at the replay seam rather than truncated (the e2e feed asserts it).
- No verdict changes.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** (`--profile industrial`, `sv verify-auto` subprocess, reads `counterexample.inputs` for replay) | every number you read today is unchanged; a cell you never saw (a wide bus) now appears with `value: null` and a `hex` string — a replay that drives `u64` must skip or decode it | apply the one-line migration where `value` is read; nothing else |
| **monono** (direct `sv verify-auto` CLI `--json`) | same | same |
| **mununu-ui** | `CexCellView` type: `value: number \| null; hex: string; bits: number` | update the TypeScript type when convenient (RTL verb — no UI obligation) |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the extractor and the wire type live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the edn e2e below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not produce traces |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | the JSON gains two fields per cell; `value` may be `null` on a wide cell |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | same |

## Measured

| run | before | after |
|---|---|---|
| `e2e_opentitan_ip_edn_full` (OpenTitan `edn` + its 15 system properties, shipped defaults) | 56 holds / 8 violated / 3 ⊥ / 0 unsupported; the violated rows' traces listed `alert_rx_i`, `clk_i`, `csrng_cmd_i`, `edn_i` — and not the 109-bit `tl_i` (the issue's observation) | **PASS, pins unchanged (56 / 8 / 3 / 0)** and the new assertion holds: a violated `EdnFatalAlertNoRsp_A` row lists `tl_i` at 109 bits; 1 431 s |
| `btor2 verify --witness` on a 65-bit input `bad` compares against 2^64 + 5 (`wide.btor2` in the briefing) | the cell dropped by the parent's `as_u64()?` filter — an empty frame | `"inputs": [ { "wide_in": "0x10000000000000005" } ]`, all seven members `violated` |
| `native_bmc::a_witness_lists_cells_wider_than_64_bits` (host) | the type does not exist on the parent; the parent's extractor drops both cells | the 65-bit input and the 100-bit state listed with their width and hex |

## Verification steps for every consumer

```bash
# a 65-bit input read by `bad`: expect the input in frame 0 as "0x10000000000000005" (btor2 verify --witness)
printf '1 sort bitvec 65\n2 input 1 wide_in\n3 sort bitvec 1\n4 constd 1 18446744073709551621\n5 eq 3 2 4\n6 bad 5\n' > wide.btor2
mununu btor2 verify --witness wide.btor2 | jq .witness.inputs
```

## Provenance

- Fix commit: pending merge — branch `fix/723-wide-trace-values`.
- Issue: [mununu#723](https://github.com/vscorza/mununu/issues/723).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md); reproducers per [`docs/policies/reproducers-before-fixes.md`](../policies/reproducers-before-fixes.md).

## Not covered here

- A full-width exact-engine witness for cells wider than 128 bits (the BDD decoder's `u128`); today such a design gets no exact witness and the BMC witness instead.
- The `btor2 game` surface (`StallLassoView` / `LassoCell`, `value: u64`) keeps narrowing `u128` to `u64` — a separate issue if a game witness ever carries a wide cell.
- The mununu-ui TypeScript type.
