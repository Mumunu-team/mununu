# Consumer briefing — `sv mutate` takes the lift-posture flags; `blackboxed_modules` lists only instantiated modules (2026-10)

> Source of truth: [`SvMutateArgs`](../../crates/mununu-cli/src/main.rs) / [`SvMutateRequest`](../../crates/mununu-core/src/api/models.rs) + [`blackboxed_module_names`](../../crates/mununu-core/src/adapter/yosys/mod.rs) — surface: (CLI+API) — `sv mutate` / `POST /api/v1/sv/mutate`, and the `diagnostics.blackboxed_modules` field of `sv verify-auto` / `POST /api/v1/sv/verify-auto`

**Audience:** rosf, monono, mununu-ui and anyone gating on `blackboxed_modules` or driving `sv mutate`. PR C of the 2026-10-20 open-issue round (mununu#724, #725); follows PR A (#745).

## TL;DR

- **`sv mutate` accepts `--lift-override NAME=PATH`, `--config-value SIGNAL=VALUE` and `--cutpoint SIGNAL`** (mununu#725), parsed by the same functions `sv verify-auto` uses, threaded into the baseline and the mutant lift alike. Every OpenTitan IP lifts only with the `prim_assert.sv` shim and `rst_ni=1`, so until now a real IP could not be mutated from the CLI at all (the full-IP mutation smoke went through the Rust API). `POST /api/v1/sv/mutate` gains the matching `lift_overrides: [{name, content}]`, `config_values: ["signal=value"]` and `cutpoint: ["net"]` fields (default empty; a malformed or hex `config_values` entry is a 400 before the lift, as on `sv/verify-auto`).
- **`sv/mutate` is now schema-pinned**: `docs/api-schemas/sv-mutate-request.schema.json` and `sv-mutate-response.schema.json`, under the same drift detector as the other wire types. The response shape is unchanged.
- **A bodiless module nothing instantiates is no longer reported in `diagnostics.blackboxed_modules`** and no longer raises the `blackbox-cut` soundness caveat (mununu#724). yosys marks any bodiless module `(* blackbox *)` in the hierarchy snapshot, including an empty `module` that only carries `@mununu_guarantee` comments; the attribute scan is now intersected with the elaborated design's cell types. A marked module that *is* instantiated, and an instantiated module with no definition (`prim_sparse_fsm_flop`), are reported exactly as before. The sidecar emission (`BlackBoxInterface.json`, contract discovery) still lists every marked module.
- No verdict changes. The `blackboxed_modules` list can only shrink, and only by names that never corresponded to a cut.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** (`--profile industrial`, `sv verify-auto` subprocess) | a consumer gate on `blackboxed_modules.is_empty()` stops failing on an annotation-only empty module; `sv mutate` can now be driven on OpenTitan-style designs with the same lift flags as `sv verify-auto` | none required; drop any "use a `package`, not a `module`" workaround at leisure |
| **monono** (direct `sv verify-auto` CLI) | same | none required |
| **mununu-ui** | the `SvMutateRequest` TypeScript type may add the three optional fields (`lift_overrides`, `config_values`, `cutpoint`); `sv mutate` is an RTL verb, so no UI affordance is required | optional |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the flags and the list live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the gates below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not run the lift |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | no rosf verdict changes |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | no monono verdict changes |

## Measured

Host: `cargo test -p mununu-cli` (the clap reproducer `sv_mutate_accepts_the_lift_posture_flags` fails on `main` with `unexpected argument '--lift-override'` and passes here), `cargo test -p mununu-core --lib --features api` (the API test `sv_mutate_handler_accepts_the_lift_posture_fields_and_rejects_hex_config_value`, the yosys test `blackboxed_module_names_keep_only_instantiated_modules`, the two new drift tests).

Image gates (`mununu-sva`, shipped defaults):

| run | before | after |
|---|---|---|
| mununu#724 probe — a 1-bit toggle design + `module empty_props; endmodule` carrying one `@mununu_guarantee`, `sv verify-auto --preprocess-sv2v --source empty.sv --json` (the sv2v + yosys path, where yosys keeps the bodiless module in the hierarchy snapshot) | `blackboxed_modules: ["empty_props"]` + a `blackbox-cut` soundness caveat (the issue's observation on the full IP) | **`blackboxed_modules: []`**, no caveat, the property extracted and decided (`holds`) |
| mununu#725 — the issue's own command on the full OpenTitan `edn` IP: `sv mutate edn.sv --top edn --preprocess-sv2v <the 63 run.sh sources> --lift-override prim_assert.sv=prim_assert_shim.sv --lift-override prim_assert_standard_macros.svh=/dev/null --lift-override prim_assert_sec_cm.svh=/dev/null --config-value rst_ni=1 --list` | `unexpected argument '--lift-override'`; without the shim the lift dies in yosys (`Found topological loop`) | **184 `stick` / 181 `drop_reset` / 43 `off_by_one` / 2151 `invert_cond` targets** listed (the discovery lift goes through the shim) |
| mununu#725 — the same command with `--source mununu_properties.sv --mutation stick:u_reg.u_err_code_edn_main_sm_err.q` (the register the full-IP mutation e2e sticks through the Rust API) | impossible from the CLI | **baseline + mutant verified, 24 min: 2 of 67 properties flip** (`ann_guarantee_1`, `ann_guarantee_4` — the error-record system properties — `holds` → `violated`; 65 unflipped), the same flip `e2e_opentitan_ip_edn_stick_flips_the_error_record` pins through the API |

## Verification steps for every consumer

```bash
# mununu#724 — an empty module beside a design: expect blackboxed_modules == []
printf 'module m(input logic clk, output logic q); always_ff @(posedge clk) q <= ~q; endmodule\n' > m.sv
printf 'module empty_props; endmodule\n' > empty.sv
mununu --quiet sv verify-auto m.sv --top m --source empty.sv --json | jq .diagnostics.blackboxed_modules
# mununu#725 — the OpenTitan edn example, mutated from the CLI with the shim
cd examples/verify/ip_opentitan_edn/source && mununu sv mutate edn.sv --top edn --preprocess-sv2v $(sed 's/^/--source /' sources.txt) \
  --lift-override prim_assert.sv=prim_assert_shim.sv --config-value rst_ni=1 --list
```

## Provenance

- Fix commit: pending merge — branch `fix/sv-mutate-lift-flags-and-blackbox-724-725`.
- Issues: [mununu#724](https://github.com/vscorza/mununu/issues/724), [#725](https://github.com/vscorza/mununu/issues/725).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md); reproducers per [`docs/policies/reproducers-before-fixes.md`](../policies/reproducers-before-fixes.md).

## Not covered here

- mununu#723 (counterexample values wider than 64 bits) — PR D, its own briefing with a migration note.
- The mununu-ui `SvMutateRequest` type (optional, RTL verb).
- `sv mutate` still has no `--auto-stub-flops` / `--predicate` / `--counter-bound` peers; none of the measured IPs needed them to mutate.
