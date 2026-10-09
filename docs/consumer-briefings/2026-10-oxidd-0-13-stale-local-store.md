# Consumer briefing — 2026-10 oxidd 0.11 → 0.13: the release-only stack overflow in the exact engine (and the CI abort) was a stale thread-local node store

> **Audience:** anyone running the exact-symbolic engine in a release binary — `sv verify-auto` / `btor2 verify*` / recoverability, directly or through the portfolio — monono's formal lane first (it pinned its engine at `bd5dd63` on this); anyone who read the `wall_class_matrix` abort as a code regression.
>
> **Related:** closes [mununu#638](https://github.com/Mumunu-team/mununu/issues/638); explains the CI abort behind the #636 revert ([#640](https://github.com/Mumunu-team/mununu/pull/640)) and the "unbounded descent" of [mununu#543](https://github.com/Mumunu-team/mununu/issues/543). Upstream: [OxiDD 9fd1ed0](https://github.com/OxiDD/oxidd/commit/9fd1ed0) "Fix stale `current_store` address in `LocalStoreStateGuard::drop`". Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **Dependency bump, verdict-semantics change (aborts → decisions), no wire-shape change.** A release binary of mununu on oxidd 0.11 could abort with `thread 'main' has overflowed its stack` (exit 134, no report) whenever the exact engine used two BDD managers on one thread — the squaring rescue (the raster twin of #638) and the cube ladder's second manager (the CI abort). The cause is upstream: OxiDD 0.11's thread-local node-store state kept a stale owner address, so allocations after switching managers corrupted the diagram and OxiDD's `apply` then recursed until the stack ended (74,548 frames on a 20-variable cone). The debug profile never showed it. oxidd 0.13 carries the fix; mununu now pins it. Nothing in mununu's engine changed.

## What changed

- `oxidd = "0.13"` (was `0.11`; `oxidd-manager-index` 0.13.1). The fix landed upstream on 2026-07-09, after 0.12.0 was cut, so no 0.11.x or 0.12 patch has it — 0.13 is the first release that does.
- Two signature adaptations in the #543 level validator (`Manager::get_node` takes a `Ref` by value; `as_edge` returns a `Copy` `Ref`). No engine behaviour, order, budget or knob changed.

## Measured — `rtl/vpu/video_timing/faulty/video_timing_early_row.sv` (monono, public), the #638 command

| binary | `sva_1` (frame wrap, raster) | process |
|---|---|---|
| release, oxidd 0.11 (the consumer's `26ec6bd`, and `main` before this PR) | — | **`thread 'main' has overflowed its stack`, exit 134, no report**; gdb: 74,548 `apply_ite` frames under `substitute` ← `box_pre` |
| release, oxidd 0.11, `MUNUNU_BDD_SQUARING=0` | ⊥ on the iteration budget | exit 2 |
| debug, oxidd 0.11 | VIOLATED (exact-symbolic) | exit 2 — the bug does not show in the profile the test suite runs |
| release, oxidd 0.13 (this PR) | VIOLATED (exact-symbolic; `sva_2` VIOLATED too — the early row also breaks `a_row_advances_only_at_line_end`; the other six HOLD) | exit 2, 8/8 decided, 3 s |

The release-profile exact-engine suite (`squaring` + `symbolic_bitblast::tests`, 99 tests) passes on 0.13; the debug suite passes unchanged. The twin and its SVA are vendored at [`examples/verify/v13_video_timing_raster_twin/`](../../examples/verify/v13_video_timing_raster_twin/) with a release-profile `#[ignore]`d e2e (`e2e_638_the_raster_twin_decides_in_release_without_overflowing`) that aborts on the parent and decides on this PR.

Also measured: release + debug-assertions + overflow-checks on 0.11 still aborts (no assertion, no overflow panic) — the manifestation is purely optimization-dependent, as it would be for a stale pointer in an `unsafe` fast path.

## What to update, per consumer

- **monono:** unpin the engine from `bd5dd63`; the #543 retry-on-abort branch of the lane stops firing on this class. The `mem_sched` single abort noted in #638 was most likely the same thing; re-run it on this binary and say so if it recurs.
- **Lanes reading exit codes:** an exit 134 on the exact engine's path is no longer expected; treat one as a new report.
- **Report parsers:** nothing changed.
- **The CI reading:** the `wall_class_matrix::config_partition_over_reset_partitions_opentitan_fsms` abort (6 of 8 CI failures since 2026-10-07) is this mechanism on the cube ladder's second manager, timing-dependent in debug through OxiDD's GC signal thread; if it recurs after this bump it is a different defect and the diagnostic branch of draft PR #644 is the instrument.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | the engine's BDD library | **Yes** |
| `mununu-dev` | test image; the crate is vendored at build | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
cargo build --release -p mununu-cli
mununu sv verify-auto rtl/vpu/video_timing/faulty/video_timing_early_row.sv \
    --source rtl/vpu/video_timing/video_timing_sva.sv --top video_timing --config-value rst_n=1   # exit 2, 8/8 decided
cargo test --release -p mununu-core --lib --features api -- squaring symbolic_bitblast::tests   # the engine suite in the profile that showed it
```

## Not covered here

- The #543 validator (`MUNUNU_VALIDATE_BDD_LEVELS=1`) still runs only on the blaster's build ops, which is why it stayed silent here; extending it to the rescue's transfers is a separate change.
- Whether the bump moves any timing (the 0.13 apply cache allows multiple values per entry) is not measured in this PR; #642 is where that question lives.
