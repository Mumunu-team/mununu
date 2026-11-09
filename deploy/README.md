# The on-prem bundle

> Source of truth: [`docker/Dockerfile.bundle`](../docker/Dockerfile.bundle), [`compose.yaml`](compose.yaml), [`scripts/bundle.sh`](../scripts/bundle.sh), and `mununu server --ui-dir` ([`main.rs`](../crates/mununu-cli/src/main.rs), [`create_router_with_ui`](../crates/mununu-core/src/api/server.rs)) — surface: CLI+API (+ the UI it serves).

Everything mununu does, on your machines, in one container on one port: the `mununu` binary, the
SystemVerilog toolchain it drives (yosys with `read_slang`, slang, Verilator, btormc, pono, cvc5 —
one pinned oss-cad-suite release, the one the e2e suite validates in), and the web UI served from
the same origin as the API (no CORS, no second port). The install is three files: the image
tarball, `mununu.toml`, and your `mununu.lic`.

## Install (air-gapped or not)

```bash
# 1. the image — from the tarball you received (no internet needed) …
docker load -i mununu-bundle-<version>.tar.gz
sha256sum -c mununu-bundle-<version>.tar.gz.sha256
# 2. the three files beside each other
mkdir mununu && cd mununu
cp /path/to/compose.yaml /path/to/mununu.toml .     # from the bundle
cp /path/to/your/mununu.lic .                         # the licence you were issued
mkdir work                                            # your designs go here (mounted at /work)
# 3. up
docker compose up -d
docker compose exec mununu mununu licence status      # → licence: valid
```

Then **http://localhost:8080** is the UI and `http://localhost:8080/api/v1/…` the API. The port is
bound to loopback by `compose.yaml`; put a TLS-terminating proxy in front to expose it to a team.

## Verify a design

Over the CLI, inside the container (designs in `./work`):

```bash
docker compose exec mununu mununu sv verify-auto /work/design.sv --source /work/design_sva.sv --top design --json
```

Over the API, from any client on the host:

```bash
curl -s localhost:8080/api/v1/health | jq .          # status, licence summary
```

Over the UI: open the page, load a CTXDSL model or an RTL design, run it — the same engines.

## What is in the image

| part | from | why |
|---|---|---|
| `mununu` (`--features api`) | built in the image's first stage on the pinned Rust toolchain against Debian trixie's libz3 4.13 | the product: CLI, API, `licence`, the server |
| oss-cad-suite (one pinned tag, see the image label `io.github.mumunu-team.oss-cad-suite.tag`) | YosysHQ's release tarball | the SystemVerilog lift and the oracles |
| `g++`, `make`, `perl` | Debian | Verilator compiles the counterexample-replay model at run time |
| the UI build | `mununu-ui`'s `npm run build` with `VITE_API_URL=/api/v1` (same origin) | the web UI at `/` |

Not in it: sv2v (the dev-only differential oracle), the e2e suite, any Rust toolchain.

The licences of everything in the image, and where each text is inside it: [`NOTICE.md`](NOTICE.md), shipped beside the tarball and at `/usr/share/doc/mununu/NOTICE.md`.

## Memory and the licence

- `mem_limit` in `compose.yaml`: the memory ceiling (`MUNUNU_MAX_PROCESS_MEMORY_BYTES`, auto) fires
  at 80 % of it and the remaining properties abstain with a note — raise the limit, not the ceiling.
- The licence file is mounted at `/etc/mununu/mununu.lic` and named by `mununu.toml`. Without it the
  bundle runs as a 30-day evaluation counted from the container's first start ([`docs/licensing.md`](../docs/licensing.md)).
- The usage ledger is **off** by default. To keep one (one JSON line per verification run: counts,
  build, licence — never design content), uncomment `ledger = …` in `mununu.toml` and the
  `./ledger:/var/lib/mununu` volume in `compose.yaml`, then `mkdir ledger`
  ([`docs/licensing.md` § The usage ledger](../docs/licensing.md#the-usage-ledger-opt-in-local)).

## Building the bundle (maintainers)

```bash
scripts/bundle.sh                 # UI build → image → dist-bundle/<version>/{tarball, sha256, compose.yaml, mununu.toml, README.md}
MUNUNU_UI_DIR=../mununu-ui scripts/bundle.sh
```

The script builds the UI (`VITE_API_URL=/api/v1`), copies it to `deploy/ui-dist/` (gitignored),
builds `docker/Dockerfile.bundle` with the build identity, `docker save`s the image and writes the
checksum. Signing the tarball is a release-key decision not taken yet; the checksum is what ships.
