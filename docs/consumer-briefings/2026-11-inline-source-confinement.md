# Consumer briefing: inline SV sources may no longer reach outside the request (2026-11)

> Source of truth: [`confine_includes_under`](../../crates/mununu-core/src/api/handlers.rs), [`staged_path`](../../crates/mununu-core/src/adapter/mod.rs) — surface: API-only — both guard a server against its requests; the CLI stages the caller's own files by basename and is unchanged.

**Audience:** anyone who sends SystemVerilog source in a request body to a mununu server. That means every `sv` route (`/api/v1/sv/*`, which take a `lift`), `POST /api/v1/sv/extract-sva`, and `POST /api/v1/context/import` with format `sv-yosys` / `yosys` (or `auto` with a `.sv` filename). In particular this covers the on-prem bundle, whose `file_root` is `/work`.

## TL;DR

- **Source names (every server).** A source's `name`, `source_name` or `lift_overrides[].name` must be a relative file name: no leading `/` and no `..`. Anything else is a `400`. Before, the server joined the name onto its temp dir unchecked, so an absolute or `..` name would **write the content outside it** (mununu#1015; read in the code, not reproduced).
- **`include` under a `file_root` (#994).** On a server with `[server] file_root` set, an inline source may `` `include `` only a quoted relative path to a file sent with the request (`` `include "defs.vh" ``, `` `include "pkg/regs.svh" ``). These forms are refused with a `400` that names each directive by source and line:
  - an absolute path;
  - a `..` component;
  - the `<…>` form;
  - a macro-built target;
  - a directive inside a `` `define `` body that would do any of the above.
- **Why.** slang resolves an absolute `` `include `` whatever its `-I` list says. When the parse then fails, its diagnostic quotes the included file's text, and the API's error body carries that diagnostic. Reproduced on slang 11.0.448 in `mununu-sva`: an include of a one-line file outside the tree printed that line in slang's error.
- **Unchanged:**
  - without a `file_root`, `include` handling is as before;
  - the CLI, which stages each file by its basename;
  - verdicts and report shapes.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Requests whose source names are paths with `/` at the front or `..` are refused. On the bundle, a source that includes a file by absolute path is refused. | Send each included file in `additional_sources` and include it by its relative name. |
| **monono** | Same. | Same. |
| **mununu-ui** | Only if it sends names that are not plain relative file names. | None expected. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| the on-prem bundle (`docker/Dockerfile.bundle`) | Yes. | It sets `file_root = "/work"`, and it ships the binary that stages sources. |
| `mununu/docker/Dockerfile` (production), `mununu/docker/Dockerfile.sva` | Yes. | They ship the `mununu` binary; the source-name check applies on every server. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Origin:** mununu#994 (found while closing #935) and mununu#1015 (found in the code while fixing #994).
- **Tests:**
  - `an_include_that_could_leave_the_search_path_is_reported`: the scan over every directive form, including comments and a `` `define `` body.
  - `an_inline_include_outside_the_request_is_refused_under_a_file_root`: the API check, with the negative control that no root means no check.
  - `a_source_name_that_would_leave_the_staging_dir_is_refused`: the staging check, including the `Path::join` behaviour that made it a defect.

## Not covered here

- **A C `#include` reaching clang through the `c-codesign` path.** The C files come from a verify config that #935 already confines to `file_root`, but an include inside one of them is still resolved by clang. That half of #994 stays open.
- **The full API round trip** (POST, then read the 400 body) was not run. What was established: the leak is reproduced at the slang level, and the error-message path that carries it was read in the code.
