# Consumer briefing: the lift runs yosys inside its staging directory; a `TMPDIR` with a space works (2026-11)

> Source of truth: [`plan_lift`](../../crates/mununu-core/src/adapter/yosys/mod.rs) and `yosys_path_arg` in the same file — surface: CLI+API — every `sv` verb that lifts (`sv verify-auto`, `sv verify-liveness*`, `sv verify-recoverability`, `sv check-fsm`, `sv lint`, `sv mutate`, `sv emit-btor2-per-module`) and their API peers

**Audience:** anyone who runs the lift in a container or on a host whose temp directory path has a space, keeps a lift cache, or matches on the include-directory refusal text.

## TL;DR

- **A temp directory with a space no longer breaks the lift (mununu#1138).** yosys now runs inside the lift's staging directory and the script names the staged files relative to it; before, the absolute staging path was written unquoted into the script and `read_slang` split it at the space (yosys's `File … not found`, naming half a path). Quoting could not fix it: `read_slang` takes a quoted token verbatim (measured on the issue).
- **A new variable, `MUNUNU_LIFT_STAGING_ROOT`:** where the lift stages its per-call directory (default the system temp dir, `TMPDIR`). For a small or `noexec` `/tmp`.
- **The lift cache's format is 4:** entries written by earlier builds miss once and are rebuilt; nothing to migrate.
- **Refusal text:** the include-directory refusal now says why (slang takes `-I<dir>` as one unquoted token); a per-module output directory with a `"` or a line break is refused, naming `--output-dir` on `sv emit-btor2-per-module`. A per-module output directory with a space is accepted (the path is quoted to `write_btor`).
- **No verdict or JSON shape change.** `diagnostics.blackboxed_modules[].source_file` still carries the user's invocation path for the primary source (the rewrite matches the staged name yosys now reports).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A container whose `TMPDIR` has a space lifts; the cache rebuilds once. | Nothing. |
| **monono** | Same. | Same. |
| **mununu-ui** | Nothing on the wire. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the lift. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the R6 review (mununu#1138); the quoting forms measured in the `mununu-sva` image on 2026-11-20 (table on the issue).
- **Tests:** `x1138_the_lift_script_names_staged_files_relative_to_the_staging_directory` (the reproducer), `a_yosys_native_path_with_a_space_is_quoted_as_one_token`, `e2e_the_lift_runs_under_a_staging_root_with_a_space`.

## Not covered here

- A caller include directory with whitespace is still refused: slang receives it as one unquoted token and there is no form that carries it. Move or rename the directory.
