# Consumer briefing — the lift cache keys every input, and a lift needs its JSON snapshots (mununu#956, #957, 2026-11)

> Source of truth: [`read_lift_snapshot`](../../crates/mununu-core/src/adapter/yosys/mod.rs#L961), [`key_include_directives`](../../crates/mununu-core/src/adapter/yosys/lift_cache.rs#L407) — surface: CLI+API (every SV lift: `mununu sv verify-auto`, `btor2`-from-SV verbs, `POST /api/v1/sv/*`).

> **Audience:** rosf and monono (they lift SV through the CLI or the API), anyone running `make e2e` with `MUNUNU_LIFT_CACHE_DIR`.
>
> **TL;DR — a bugfix on two paths.** (1) Under `MUNUNU_LIFT_CACHE_DIR`, the cache could serve the model of an *older* input: a file `include`d by absolute path, a change in invalid UTF-8 bytes, a rebuilt slang plugin or yosys binary were not in the key. They are now (cache format 3, so every old entry misses once); an input the key cannot see is a bypass with a reason. (2) A lift whose yosys run wrote no `hier.json` or `clocks.json` used to continue with both read as empty — the multi-clock refusal and the black-box scan were silently off. It is now a lift error naming the file. No JSON shape or route changes.

## What changed

| item | before | now |
|---|---|---|
| absolute `` `include "/…" `` | not keyed: an edit served the old lift | keyed by content, recursively |
| `` `include `MACRO `` or a `..` path | not keyed | cache bypassed, `warn` names the directive |
| include-dir file contents | keyed through a lossy UTF-8 decode | keyed byte for byte |
| slang plugin named by bare name | `unresolved` when not at the image path | resolved next to yosys (`../share/yosys/plugins`); not found = bypass |
| yosys identity | `-V` banner, memoised per process | banner + binary size and mtime, memo keyed on them |
| `hier.json` / `clocks.json` missing after yosys succeeded | read as empty; checks skipped; result cached | lift error (`yosys ran but its … snapshot … cannot be used`), never cached |

## Per consumer

| consumer | what this means for you | action |
|---|---|---|
| **rosf** | Only a broken toolchain (a yosys that exits 0 without writing the snapshots) changes outcome: from a model with two checks off to an error. With the cache on, the first run after upgrading is a miss. | None. |
| **monono** | Same as rosf. Designs that `include` by absolute path now re-lift when the included file changes. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` / the release archives | Yes. | The fix is in the binary. |
| the on-prem bundle | Yes. | Same binary. |
| rosf `docker/Dockerfile*` | Only to pick up the fix. | Consumer. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only to pick up the fix. | Consumer. |

## Provenance

Found by the full tier-B review of 2026-11-09 (#956, #957). Reproducers in `adapter/yosys/mod.rs` (`an_edit_to_an_absolutely_included_file_moves_the_lift_cache_key`, `an_include_the_key_cannot_resolve_bypasses_the_lift_cache`, `a_lift_whose_json_snapshots_are_missing_is_an_error`) and `lift_cache.rs` (`include_files_differing_only_in_invalid_utf8_key_apart`), each checked to fail on the parent.

## Not covered here

Whether slang also resolves a relative include against the process working directory (not checked; such a file would not be keyed); the e2e re-run of the cache under `mununu-sva`.
