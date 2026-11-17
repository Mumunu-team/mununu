# Consumer briefing: a verify config's C sources may no longer `#include` outside the server's root (2026-11)

> Source of truth: [`confine_c_includes_under`](../../crates/mununu-core/src/api/handlers.rs), [`unconfined_c_includes_in_tree`](../../crates/mununu-core/src/codesign/include_scan.rs) — surface: API-only — it guards a server against its requests; the CLI compiles the caller's own files and is unchanged.

**Audience:** anyone who sends `POST /api/v1/verify` with a config that has a `c-codesign` source to a mununu server that sets `[server] file_root`. In particular the on-prem bundle, whose `file_root` is `/work`. A server without a `file_root` is unchanged.

## TL;DR

This closes the C half of mununu#994 (the SV half shipped in the inline-source briefing, #1016). On a server with a `file_root`, every `#include` / `#include_next` / `#import` in a `c-codesign` source, and in every header it reaches under the root (through the including file's directory, then the config's `include_paths`), must be a quoted or angled relative path of plain components: `#include "regs.h"`, `#include "hal/gpio.h"`, `#include <stdint.h>`. These are refused with a `400` that names each directive by file and line, before clang runs:

- an absolute path (`#include "/etc/hostname"`, `#include </etc/passwd>`);
- a `..` component;
- a macro-built target (`#include HEADER`);
- a directive with no argument.

Before, clang resolved an absolute include whatever its `-I` list said, and a failed compile quotes the included text in the diagnostics the report's error carries, so a C file under the root could read a file outside it back to the caller.

What does not change: a header the scan cannot find under the root (a system header such as `<stdint.h>`, or anything on the compiler's own search path) is not followed; `include_paths` and `files` themselves were already confined to the root by #935; a commented-out directive is not one; the CLI (`mununu verify`, `mununu codesign extract-c`) is untouched.

## Per consumer

### rosf · monono (HTTP)

- A `POST /api/v1/verify` whose C sources, or the headers they reach, carry one of the forms above now fails with a `400` whose message starts `a server with a file_root compiles a c-codesign source only when every #include it reaches is …; refused: <file>:<line>: #include <arg>; …`. Rewrite the include as a relative path to a file under the root (or send the header with the project).
- No response field, route or verdict value changes. A request that passed before and used only relative includes passes unchanged.

### mununu-ui

No CTXDSL surface; no change required.

## Docker rebuild

| image | rebuild | why |
|---|---|---|
| `mununu` (product) | yes | the server-side check |
| `mununu-sva` (dev / e2e) | yes | same |
| consumers' own images | no | no schema change; `docs/api-schemas/` untouched |

## Provenance

Found while closing #935 (2026-11-11), reproduced for SV on slang 11.0.448 in `mununu-sva` (#1016). The C half is the same class on clang: structural (read in the code), with the scanner and the handler seam pinned by tests (`include_scan::tests`, `a_c_include_outside_the_root_is_refused_under_a_file_root`); a clang-level reproduction needs the `mununu-sva` image and is not required for the refusal, which runs before clang.

## Not covered here

Includes that reach a file outside the root through a symlink under it (the scan follows the compiler's view of the path; `confine_under` canonicalises what the config names, and the scan only follows what canonicalises under the root). A config's `extra_clang_args` naming `-include` / `-imacros` paths: those are config-named paths and belong to #935's rule, not to this one.
