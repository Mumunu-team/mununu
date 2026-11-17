# Consumer briefing: the owned SAT BMC joins the reach portfolio (2026-11)

> Source of truth: [`run_sat_bmc`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) — surface: CLI+API — every verb that runs the reach portfolio (`btor2 verify`, `--owned-only`, `sv verify-auto`, the liveness rungs' l2s step) on the CLI and the HTTP API

**Audience:** anyone who reads a reach-portfolio verdict's member lists (`reachable_by`, `refused`, `abstained`, `undecided`, `portfolio.decided_by`).

## TL;DR

- **A new member, `sat-bmc`.** It is the owned bit-level BMC with the dynamic step (`--bmc-engine sat-dyn`, mununu#1035), now run inside the reach portfolio. It finds counterexamples only and never proves.
  - It runs in the **full portfolio**, beside `ric3-bmc`, under the subprocess wall, and it is cancelled when another member decides.
  - It also runs in **`--owned-only`**, beside the deep-CEX member. It is the first owned member to reach the deep counterexamples whose shallow frames are hard.
  - It is **not** in the proof-directed set used by the k-liveness rung.
- **Member lists can now name `sat-bmc`:**
  - `reachable_by` when it finds the counterexample;
  - `undecided` when it refutes every depth up to 1,000;
  - `abstained` when it stops at the wall;
  - `refused` (reason prefixed `sat-bmc:`) when the AIG backend cannot model the design (arrays).
- **Verdicts:** a row that only `ric3-bmc` decided may now also credit `sat-bmc`. `--owned-only` can decide rows it used to leave `unknown`. No definite verdict changes value: every member is sound, and a disagreement still raises the contradiction alarm.
- **No JSON shape change.** The lists are the same fields with one more possible name.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `sat-bmc` may appear in the portfolio member lists. | If you match member names against a fixed set, add `sat-bmc`. |
| **monono** | Same. | Same. |
| **mununu-ui** | Same, where the attribution is displayed. | None unless a member name is matched exactly. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the member. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Standalone reach**, measured 2026-11-14 with `--bmc-only --bmc-engine sat-dyn` on HWMCC'20, every counterexample replayed by `btorsim -c` (mununu#1035): `mul9` depth 126, `circular_pointer_top_w32_d32_e0` depth 58, `krebs.3.prop1-func-interl` depth 96. The owned-only driver without this member decided none of the three at 300 s on the box (item 6, F0: 19 / 68).
- **Join cost:** the AIG model build has no cancellation point, and the full portfolio joins its members. Parse, build and depth 0 took at most 0.71 s on all 136 HWMCC'20 bv files, with a release build on the laptop.
- **Tests:** `the_sat_bmc_member_finds_counterexamples_never_proves_and_names_every_stop`, `the_portfolio_credits_the_sat_bmc_member_and_carries_its_bound`, `the_owned_driver_names_the_members_still_running_at_its_deadline`.

## Not covered here

- **Retiring `ric3-bmc`** in favour of `sat-bmc` needs `sat-bmc` to reach `circular_pointer_top_w16_d64_e0` (first bad at depth 261), which it does not yet.
- **The box gate** (the 68-row set with the full portfolio and with `--owned-only`, plus the wall-class matrix) is recorded in the private scoreboard when it lands.
