# Consumer briefing: a `/api/v1/verify` config's file paths are confined to the server's `file_root` (2026-11)

> Source of truth: [`confine_config_paths`](../../crates/mununu-core/src/api/handlers.rs), [`VerifyConfig::try_map_paths`](../../crates/mununu-core/src/verify/config.rs) — surface: API-only — the confinement guards a server against its requests; the CLI reads the caller's own files and has nothing to confine.

**Audience:** anyone who calls `POST /api/v1/verify` on a server with `[server] file_root` set: the on-prem bundle (`/work`), and rosf, monono or mununu-ui when they deploy it.

## TL;DR

- **On a server with a `file_root`**, every file path inside the verify config must exist and resolve, after `..` and symlinks, to a location under the root, or the request is a `400` naming the field (`sources[0].files[1]`, `sources[2].options.include_paths[0]`, `alphabet.register_map`, `templates.files[0]`). A relative path still resolves against `base_dir`, as before. The paths covered are:
  - `[[sources]] files`;
  - the `sidecar`, `include_paths` and `register_map` source options;
  - `[alphabet] register_map`;
  - `[templates] files`.
- **A config that sets `options.clang` is refused** on such a server, because a request does not choose the executable the server runs. Configure clang on the server's `PATH` instead.
- **Before:** `base_dir` was confined (#970), but an absolute path inside the config passed straight through the orchestrator's join, so a request could read any file the server could.
- **Unchanged:** without a `file_root`, which is every server but the bundle, nothing changes. The verdicts, the report shape and the file names in the report are the same, since a path inside `base_dir` stays relative.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Only if you call the bundle's `/api/v1/verify` with paths outside `/work` or with `clang` set. | Keep designs under the mounted `/work`; drop `options.clang`. |
| **monono** | Same. | Same. |
| **mununu-ui** | The UI sends `base_dir` and the config it loaded; a `400` now names the field to fix. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | Only if you set `file_root` in its `mununu.toml`. | The confinement applies only under a root. |
| the on-prem bundle (`docker/Dockerfile.bundle`) | Yes. | It sets `file_root = "/work"`, so this change is what closes the gap there. |
| `mununu/docker/Dockerfile.sva`, `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | No `file_root`. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | No, unless they deploy the bundle. | Same. |

## Provenance

- **mununu#935** (found by review session R0), the remainder after #970.
- **Tests:**
  - `api::handlers::context_sanity_tests::config_paths_are_confined_to_the_file_root`. It checks each visited field with `..`, an absolute path and a symlink out, plus the `clang` refusal. It also shows that a path inside the root stays as written. The negative control: with no root, every escaping config is left untouched.
  - `verify::config::tests::every_option_key_the_orchestrator_reads_is_classified`, the seam test. It reads every option key the orchestrator reads and requires each to be classified as a path, an executable or neither.

## Not covered here

- Paths named by file *content*: an SV `` `include `` or a C `#include` with an absolute path is resolved by slang or clang, not by the config.
- The window between the check and the read: the check canonicalises each path, but a file under the root that is replaced by a symlink after the check is still followed.
