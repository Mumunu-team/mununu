# Consumer briefing: the per-module and multi-module SV lift sees submodules again (2026-11)

> Source of truth: [`collapse_slang_instance_modules`](../../crates/mununu-core/src/adapter/yosys/mod.rs#L2160) — surface: CLI+API — `sv emit-btor2-per-module`, and `mununu verify` / `POST /api/v1/verify` with an `sv-yosys` source set to `multi_module = true`.

**Audience:** rosf, monono, mununu-ui, and anyone who lifts a multi-module SystemVerilog design one module at a time or composes it from its instances.

## TL;DR

- **Since the slang-only lift (#847, v0.5.10) these paths found no submodules.** `read_slang` flattens the elaborated design into its top unless asked not to, so the discovery snapshot held one module:
  - `sv emit-btor2-per-module` wrote only the top's BTOR2.
  - Multi-module composition (`multi_module = true`) failed with `adapter/yosys multi-module: no liftable instances to compose`.
- **Fixed.** The discovery pass now runs `read_slang --keep-hierarchy`. The per-instance modules yosys-slang names `<type>$<instance path>` are read as their module type, the shape the `read_verilog` discovery had before #847.
  - `emit-btor2-per-module` writes one BTOR2 per module type again.
  - Composition builds its product again.
  - Measured on `examples/systemverilog/multi_producer_consumer_top.sv` in `mununu-sva`.
- **Unchanged:** the whole-design lift (`sv verify-auto`, `sv verify-bad`, `context import` with `sv-yosys`) never used the discovery pass. Its BTOR2 and verdicts are untouched.
- **No shape, flag or route changes.** The error above goes away, and the per-module outputs and composition results come back.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | If you composed SV designs with `multi_module = true` on v0.5.10–v0.5.11, those runs errored; they produce a composition again. | Re-run any composition that failed with "no liftable instances". |
| **monono** | You use the whole-design lift (`sv verify-auto`); nothing changes. | None. |
| **mununu-ui** | The per-module path is not wired in the UI. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle | Yes, if you use the per-module or multi-module paths. | The fix is in the binary. |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | Yes, for the same paths in the image. | The binary on the volume changes; the toolchain does not. |
| `mununu/docker/Dockerfile.dev` | Only if your dev workflow lifts per module. | — |
| `mununu/docker/Dockerfile.profile`, `Dockerfile.extract*` | No. | They don't lift SV per module. |
| rosf `docker/Dockerfile*` | Yes, if rosf composes SV designs per module. | Picks up the fix. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | No. | Not callers of this path. |

## Provenance

- **mununu#981.** Four tests failed in `mununu-sva` and passed on the GitHub runner, which has no yosys and so skipped them: `compose_sv_multi_module_producer_consumer_top`, `per_module_with_connectivity_surfaces_shared_net`, `sv_yosys_multi_module_verify_resolves_qualified_atoms` and `translates_three_multi_module_fixtures_per_submodule`.
- **Tests:** the seam test `adapter::yosys::tests::a_keep_hierarchy_snapshot_reads_as_module_types` (the snapshot shape yosys 0.68+120 writes, with a negative control on the raw snapshot), and the four tests above in the image. The `adapters` test binary passes 47/47 there.

## Not covered here

- Two instances of one module type with different parameters still lift as the type with its default parameters, as before #847 (yosys's `$paramod` instances were skipped then too).
- Why a CI run could not see this: mununu#981's gate question, in its own PR.
