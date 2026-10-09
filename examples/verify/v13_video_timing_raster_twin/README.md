# `video_timing` raster twin — the mununu#638 reproducer

> Source of truth: [`e2e_638_the_raster_twin_decides_in_release_without_overflowing`](../../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: CLI+API (`sv verify-auto`, `POST /api/v1/sv/verify-auto`)

Vendored from [`Mumunu-team/monono`](https://github.com/Mumunu-team/monono) (`rtl/vpu/video_timing`, Apache-2.0), unchanged:

- `video_timing_early_row.sv` — the 640×480 sync generator's **contrast twin**: the frame wraps one row early, so `a_frame_wraps_at_total` (`sva_1`) must be VIOLATED. Two 10-bit counters, no data inputs; the whole frame is the diameter (800 × 525 clocks), the iteration-bound wall class the squaring rescue exists for.
- `video_timing_sva.sv` — the five tier-1 safety assertions, bound by module name.

**Why it is here.** On oxidd 0.11 a *release* binary aborted on this command with `thread 'main' has overflowed its stack` (exit 134, no report): the squaring rescue is the one place the exact engine uses two OxiDD managers alternately on one thread, and OxiDD's thread-local node-store state kept a stale owner address across that switch (upstream 9fd1ed0, fixed in 0.13), so the diagram was corrupted and `apply` descended 74,548 frames on a 20-variable cone. The debug profile never showed it, which is why the e2e that uses this fixture is run **in release**:

```bash
docker run --rm -v "$(pwd)":/work -w /work -v mununu-target:/cargo-target \
  mununu-sva cargo test --release -p mununu-core --lib --all-features -- --ignored e2e_638_
```

Expected on `main`: 8/8 decided by exact-symbolic, `sva_1` VIOLATED, `sva_2` VIOLATED (the twin's early row also breaks `a_row_advances_only_at_line_end`), the rest HOLD. On the parent of the fix the process aborts instead.

Issue: [mununu#638](https://github.com/Mumunu-team/mununu/issues/638). Briefing: [`docs/consumer-briefings/2026-10-oxidd-0-13-stale-local-store.md`](../../../docs/consumer-briefings/2026-10-oxidd-0-13-stale-local-store.md).
