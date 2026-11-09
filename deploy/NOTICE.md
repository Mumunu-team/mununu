# Notices in the mununu on-prem bundle

> Source of truth: [`docker/Dockerfile.bundle`](../docker/Dockerfile.bundle) — surface: CLI-only — the bundle is an image built by `scripts/bundle.sh`

This image holds the work of several projects. Each part stays under its own licence; this file
says where each text is. [`THIRD_PARTY.md`](../THIRD_PARTY.md) § "Bundled in the on-prem image"
has the full table.

| part | where in the image | licence texts |
|---|---|---|
| mununu (`/usr/local/bin/mununu`) | — | `/usr/share/doc/mununu/LICENSE`, `THIRD_PARTY.md`, `LICENSES/`, `LICENSING.md` |
| oss-cad-suite (yosys, slang, Verilator, btormc, pono, cvc5, …), unmodified | `/opt/oss-cad-suite/` | `/opt/oss-cad-suite/license/` (one file per component) |
| Debian base system and installed packages | the system tree | `/usr/share/doc/<package>/copyright` |
| the web UI | `/srv/mununu-ui/` | `/srv/mununu-ui/LICENSE.txt`, `/srv/mununu-ui/THIRD_PARTY_NOTICES.txt` |

Commercial use of mununu needs a licence file and an order form; see `LICENSING.md`.
