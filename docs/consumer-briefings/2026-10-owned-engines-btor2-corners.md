# Consumer briefing — the owned engines read every BTOR2 corner the same way; a malformed property is refused, never improvised on (2026-10)

> Source of truth: [`operand_bv`](../../crates/mununu-core/src/adapter/btor2/native_bmc.rs) (the shared operand resolver of the native BMC, SPACER and interpolation members) + [`validate`](../../crates/mununu-core/src/adapter/btor2/parser.rs) (the BTOR2 parser's acceptance rule) — surface: (CLI+API) — `btor2 verify` / `btor2 spacer-check` / `POST /api/v1/btor2/verify`, and every `sv verify-auto` row the reach portfolio decides

**Audience:** rosf, monono, mununu-ui and anyone pinning `btor2 verify` or `sv verify-auto` verdicts. PR A of the 2026-10-20 open-issue round (mununu#741, #742, #743, #730, and the negated-operand defect found on the way); PR B (mununu#740, SPACER corroborate-only) follows.

## TL;DR

- **Soundness — an `init` whose value is an input, another state, or an expression over them now pins the cell.** BTOR2 permits a non-constant `init` (a shared nondeterministic initial value); the z3 encoders of the native BMC, the deep counterexample search and SPACER resolved an `init` value through the operator/constant table only, so such an `init` was silently dropped and the cell left **free** — an over-approximation of the initial states that produced a **spurious `violated`** (`--owned-only` reported it outright; the full portfolio ended in a contradiction alarm). Those rows now read `unreachable` / `holds` (mununu#741). yosys never emits a non-constant `init`, so `sv verify-auto` rows are unaffected; hand-written and HWMCC designs through `btor2 verify` are.
- **Soundness — a negated operand (`bad -N`, `constraint -N`, `init … -N`, BTOR2's bit-not shorthand) keeps its sign** in every owned reader: the native BMC / k-induction, SPACER and interpolation members read `bad` / `constraint` / `init` operands by their absolute id, i.e. the **opposite** property (a design whose `bad` is `¬s` was reported `violated` at depth 0; a design whose `init` is `¬0` was proved SAFE by k-induction); the **exact engine's initial-state BDD** and the cube lift's pinned initial valuation, the enum / counter detectors and the smart-init pin resolved a negated `init` constant to the un-negated value (the exact engine answered `unreachable` on a design violated at step 0 — the image run of this PR caught it as a contradiction against the fixed z3 members). The subprocess members (btormc, pono, rIC3) already read the sign. yosys emits `not` nodes, not negated operands, so `sv verify-auto` is unaffected.
- **Completeness — SPACER decides an input-dependent `bad` or `constraint`.** Its Horn rules bound the state variables only, leaving an input a free constant shared across the rules; z3 answered `unknown` on every input-dependent monitor (`bad = in ∧ ¬s`, `(cnt == 3) && in`, a stateless `bad = in`). The inputs are now bound in every rule and a stateless design gets a 0-ary invariant (mununu#742). Until PR B lands a lone SPACER `unreachable` is still trusted, so a row that only SPACER now decides carries the trust caveat of mununu#740 — read `unreachable_by: ["spacer"]` with that in mind.
- **Refusal — a malformed BTOR2 file is a parse error for every verb** (mununu#743): a `bad` / `constraint` / `fair` / `justice` operand wider than 1 bit (btor2tools: `expected bitvec of size 1 for bad but got 4`), a non-positive id, or a combinational cycle (an operator whose operand chain leads back to itself, expressible through a forward reference). Before: the exact engine silently took bit 0, the z3 engines panicked on a sort mismatch (read as abstentions inside the portfolio), and a cycle overflowed the z3 encoder's recursion — an abort. `btor2 verify` exits with the parse error, the API answers 400, `btor2 spacer-check` prints `unknown` (it abstains on any parse failure, by design). Line order is **not** policed (mununu's own rewrites append a sort after its users); btor2tools consumers keep receiving the renumbered `emit_btor2_for_subprocess` text.
- **Robustness — the exact engine no longer panics when OxiDD runs out of arena** during a `¬`, `∧`, `∨`, a restriction conjunction or the constraint-satisfiable projection (mununu#730): every fallible apply propagates the existing `arena-overflow` abstention (`raise MUNUNU_BDD_ARENA_NODES, or use --engine explicit`), attributed in the report. The two census panics on `prim_esc_receiver` were this path.
- No JSON shape change. No CLI flag or route change.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** (`--profile industrial`, `sv verify-auto` subprocess) | none of your RTL rows change (yosys inits are constants, no negated operands); a hand-written BTOR2 with a wide `bad` now fails to parse instead of deciding on bit 0 | rerun; no pins move |
| **monono** (direct `sv verify-auto` CLI) | same | rerun the lane at the next pin |
| **mununu-ui** | none | none |
| anyone running `btor2 verify` on HWMCC / hand-written designs | a design with a non-constant `init` or a negated property operand may flip from `violated` to `unreachable` (or the reverse for a negated `init`) — the new verdict is the sound one; a malformed file is refused with a line-numbered message | re-check pinned verdicts on such designs |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the fixes live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the e2e / portfolio gates below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not run the engines |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | no rosf row changes |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | no monono row changes |

## Measured

Host (`cargo test -p mununu-core --lib`): the nine reproducers below fail on `main` @ `dff5c90` and pass on this branch; the rest of the suite is unchanged except 21 hand-written test fixtures whose `bad` / `fair` / `justice` operand was wider than 1 bit (now retagged — no checked-in `.btor2` file had either defect).

| reproducer | engine(s) | `main` | this branch |
|---|---|---|---|
| two states initialised from the same input, `bad = (s != t)` | native BMC, deep cex, k-induction, SPACER | `Violated{0}` (spurious) | `NoCexWithin` / `Safe` |
| `t` initialised from `¬v` (an operator over the input); `t` initialised from the state `s` | same | `Violated{0}` (spurious) | `Safe` |
| `bad -2` with `s ≡ 1`; `constraint -in` | native BMC, k-induction, SPACER | `Violated{0}` (spurious) | `Safe` |
| `init s = -zero`, `bad = s` | native BMC, k-induction, SPACER; the exact engine (`exact_bad_reachable`); the detectors' and the cube lift's init constant | `Safe` / `unreachable` / `0` (spurious) | `Violated{0}` / reachable / `0b11` |
| `bad = in ∧ ¬s`, `bad = in ∧ s`, stateless `bad = in`, `constraint ¬in` | SPACER | `Unknown` | `Violated{0}` / `Safe` / `Violated{0}` / `Safe` |
| a 4-bit `bad`; a 4-bit `constraint` / `fair` / `justice`; a combinational cycle; id `0` / `-1` | parser | accepted | `ParseError` |

Image gates (`mununu-sva`, shipped defaults):

| run | before | after |
|---|---|---|
| HWMCC'20 bv adjudication (`hwmcc_adjudication_over_user_dir`, full portfolio, the 42 instances before the mununu#738 crasher, user-downloaded corpus) | 5 reachable / 20 unreachable / 17 unknown (PR #739) | **7 / 25 / 10** — every verdict agrees with the official per-solver results (0 label mismatches, 0 contradictions); `vis_arrays_am2901` moves from the wrong lone-SPACER `holds` (mununu#740) to `Reachable` by rIC3; four rows are decided by SPACER alone (`gen31`, `gen35`, `gen44`, `h_RCU`, all ground-truth safe) — the exposure PR B removes |
| the eight reproducer designs through `btor2 verify --owned-only`, `--bmc-only`, `btor2 spacer-check`, with `rIC3 -e ic3` as the oracle | `--owned-only` contradiction on `init s = -zero` (exact: unreachable); SPACER `unknown` on the input-dependent `bad`s; the wide `bad` decided on bit 0 | every verdict agrees with rIC3; the wide `bad` and the cycle are parse errors (rIC3 itself aborts on the cycle) |
| `e2e_opentitan_ip_edn_full` (OpenTitan `edn` + its 15 system properties, shipped defaults) | 56 holds / 8 violated / 3 ⊥ / 0 unsupported (PR #739) | **PASS, pins unchanged** (holds floor 56, the 8 `EdnFatalAlertNoRsp_A` violations with counterexamples, the three `AG EF` rows ⊥), 1 161 s beside a concurrent user simulation |

## Verification steps for every consumer

```bash
# a non-constant init (mununu#741) — expect unreachable / holds, not violated
printf '1 sort bitvec 1\n2 input 1 v\n3 state 1 s\n4 init 1 3 2\n5 next 1 3 3\n6 state 1 t\n7 init 1 6 2\n8 next 1 6 6\n9 neq 1 3 6\n10 bad 9\n' > init_from_input.btor2
mununu btor2 verify --owned-only init_from_input.btor2
# a wide bad (mununu#743) — expect a parse error naming node 8 and its width
printf '1 sort bitvec 4\n2 zero 1\n3 state 1 cnt\n4 init 1 3 2\n5 one 1\n6 add 1 3 5\n7 next 1 3 6\n8 and 1 5 3\n9 bad 8\n' > wide_bad.btor2
mununu btor2 verify wide_bad.btor2
```

## Provenance

- Fix commit: pending merge — branch `fix/owned-engines-btor2-corners`.
- Issues: [mununu#741](https://github.com/vscorza/mununu/issues/741), [#742](https://github.com/vscorza/mununu/issues/742), [#743](https://github.com/vscorza/mununu/issues/743), [#730](https://github.com/vscorza/mununu/issues/730).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md); reproducers per [`docs/policies/reproducers-before-fixes.md`](../policies/reproducers-before-fixes.md).

## Not covered here

- mununu#740 — a lone SPACER verdict is still trusted in the `unreachable` direction; PR B demotes SPACER to corroborate-only in both directions and names the measured cost.
- mununu#738 (the adjudication harness crash on `vis_arrays_bufferAlloc`) and mununu#744 (the native BMC's shallow-witness search) — PR B and a later measurement.
- The 3-valued cube evaluator (`AbstractRelation` in `symbolic_bitblast.rs`) has the same infallible-helper pattern as the exact evaluator had; a separate issue tracks it.
- The exact engine's witness path still caps a cell at 128 bits — PR D (mununu#723).
