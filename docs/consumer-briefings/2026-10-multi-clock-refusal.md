# Consumer briefing — a multi-clock design is refused at lift time; `--merge-clock-domains` opts in (2026-10)

> Source of truth: [`clock_domains`](../../crates/mununu-core/src/adapter/yosys/mod.rs) — surface: (CLI+API) — every SV verb that lifts (`sv verify-auto`, `sv verify`, `sv verify-liveness`, `sv verify-liveness-all`, `sv verify-recoverability`, `sv check-fsm`, `sv lint`, `sv mutate`; their `/api/v1/sv/*` routes)

**Audience:** rosf, monono, mununu-ui and anyone who lifts SystemVerilog with mununu. Fixes mununu#758.

## TL;DR

- **Soundness fix, behaviour change.** A design whose flops sit on more than one clock domain — two clock nets, or both edges of one net — was lifted as if every domain ticked on the BTOR2 model's one implicit clock. That hides every crossing defect, so a HOLDS on such a design did not transfer. Only clock-shaped input NAMES (`clk`, `clk_i`, …) were ever refused, and `clk_src` / `clk_dst` passed silently. The lift now counts the domains of the flattened netlist and refuses more than one, naming them.
- **Opt-in.** `--merge-clock-domains` (API `merge_clock_domains: true`) keeps the old reading as an explicit choice: `diagnostics.merged_clock_domains` lists the domains, and the `clock-domains-merged` soundness-caveat note says a HOLDS does not transfer.
- **New request field / flag:** `merge_clock_domains` on `SvVerifyAutoRequest`, `SvMutateRequest`, `SvVerifyRequest`, `SvVerifyLivenessRequest`, `SvVerifyLivenessAllRequest`, `SvVerifyRecoverabilityRequest`, `SvCheckFsmRequest`, `SvLintRequest`; `--merge-clock-domains` on the matching CLI verbs. **New response field:** `diagnostics.merged_clock_domains` (omitted when empty). Schemas regenerated.
- A single-clock design, including one clocked entirely on the falling edge, lifts as before.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | a lane on a multi-clock design now ends in a lift error (exit 1, `skipped` in rosf) instead of verdicts | decide per design: verify each domain separately, or pass `--merge-clock-domains` and report the caveat |
| **monono** | same for any multi-clock block | same |
| **mununu-ui** | new optional request field, new optional diagnostics field | none required (RTL verbs are CLI+API) |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the check lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the SV lift runs here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | multi-clock lanes |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | same |

## Measured

| run | result |
|---|---|
| two-clock reproducer (`a` on `clk_src`, `b <= a` on `clk_dst`, `assert b == $past(a)` @ `clk_dst`) | parent: `HOLDS`, no caveat (unsound: false on the real design); here: refused at lift, naming both domains |
| `e2e_two_clock_design_is_refused_at_lift` (mununu-sva) | refused without the opt-in; lifted with `merge_clock_domains`; the same design on one clock lifts — passes |
| `e2e_opentitan_sva_census` | 173 holds / 11 skipped / 7 violated / 0 unknown — identical to the baseline. A first cut that read the clock snapshot straight after `flatten` refused `prim_esc_sender` (`clk_i` and its sub-instance port `u_decode_resp.clk_i` read as two domains); the snapshot is now taken on an `opt_clean`ed copy of the design, which folds port aliases and `$buf` cells |
| `e2e_opentitan_ip_edn_full` | passes (626 s, unloaded host; same as the parent's 632 s). Under heavy concurrent host load the run instead attributed `edn_ack_sm_{0,7}_recover_idle` to `exact-symbolic` rather than the pinned `trap-certificate` (verdict unchanged, `violated`): `decided_by` on these rows is load-sensitive |
| `diff_corpus_monotone_verdict_ledger` | passes |

## Verification steps for every consumer

```bash
mununu sv verify-auto design.sv --top top --frontend slang            # multi-clock → error naming the domains
mununu sv verify-auto design.sv --top top --frontend slang --merge-clock-domains --json \
  | jq '.diagnostics.merged_clock_domains, [.notes[] | select(.kind=="clock-domains-merged")]'
```

## Provenance

- Fix commit: pending merge — branch `fix/multi-clock-refusal` (stacked on `fix/cover-under-assume`, #759).
- Issue: mununu#758.
- Docs: [`docs/verifying-rtl.md`](../verifying-rtl.md) §"One clock per model", [`wiki/Adapter-Formats.md`](../../wiki/Adapter-Formats.md).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- Native multi-clock verification (a per-domain enable rewrite with ratio constraints) and structural CDC lint: separate, planned work.
- `sv cegar` gets the refusal without the opt-in. The per-module KMTS path (`sv emit-btor2-per-module`, `per_module_btor`) lifts each submodule on its own and is not checked.
