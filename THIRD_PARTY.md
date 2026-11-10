# Third-party files in this repository

> Scope: every directory below holds files that are the work of others, or derived from it.
> They are **not** under the [Mununu Non-Commercial License](LICENSE); each stays under the
> licence named in its row, whose text is in [`LICENSES/`](LICENSES/) or beside the files. Everything
> not listed here is first-party and under `LICENSE`. The test
> `crates/mununu-core/tests/policy/third_party_inventory_g1.rs` fails when a directory with a
> third-party licence header or a provenance file is missing from this page.

**What you may do with one of these files.** Find its directory in the tables; the row names the
licence. For Apache-2.0 (OpenTitan, Ibex, Caliptra) the terms are
[`LICENSES/Apache-2.0.txt`](LICENSES/Apache-2.0.txt), §4 of which asks a redistributor to carry the
licence text, the notices and a note of any change: the pin file in each directory lists the
files, their upstream paths and every local change. For BSD-3-Clause (nrfx) the text is beside the
files; for MIT (btor2tools) it is quoted in the suite's `PROVENANCE.md`.

Rust crate dependencies are not vendored; `cargo deny` (`deny.toml`) checks their licences at CI.

## Vendored RTL, pinned to an upstream commit

Each directory carries `UPSTREAM_COMMIT.txt`: the upstream repository and commit on its first
line, then one line per file with its upstream path; files marked `LOCAL:` are lift-only variants
of an upstream file (an `ifdef INC_ASSERT` region stripped, a one-line syntax change), each named
with its change. Files with no licence header in these directories are first-party stubs whose
headers say so. Where the first line holds only the commit, the repository is the one the
directory is named after.

| directory | upstream | commit | licence | contents | provenance |
|---|---|---|---|---|---|
| `examples/verify/dc_lowrisc_ibex_controller/source` | lowRISC/ibex | `c6edaa4060b1` | Apache-2.0 | 2 Apache-2.0 files; 2 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/dc_opentitan_aes_cipher_control_fsm/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 4 Apache-2.0 files; 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/dc_opentitan_aes_ctr_fsm/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 4 Apache-2.0 files; 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/dc_opentitan_csrng_main_sm/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 5 Apache-2.0 files; 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/dc_opentitan_edn_main_sm/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 6 Apache-2.0 files; 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/dc_opentitan_keymgr_ctrl/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 10 Apache-2.0 files; 9 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/dc_opentitan_otbn_start_stop_control/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 16 Apache-2.0 files (1 local lift-only variants, listed in the pin file); 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/dc_opentitan_prim_alert_sender/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 10 Apache-2.0 files (1 local lift-only variants, listed in the pin file); 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/dc_opentitan_prim_arbiter_ppc/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 3 Apache-2.0 files; 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/dc_opentitan_prim_arbiter_tree/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 1 Apache-2.0 files; 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/dc_opentitan_prim_count/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 2 Apache-2.0 files; 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/dc_opentitan_prim_esc_receiver/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 11 Apache-2.0 files; 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/dc_opentitan_prim_esc_sender/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 6 Apache-2.0 files; 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/dc_opentitan_prim_fifo_sync/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 6 Apache-2.0 files; 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/dc_opentitan_prim_packer_fifo/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 1 Apache-2.0 files; 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/dc_opentitan_rom_ctrl_fsm/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 12 Apache-2.0 files (1 local lift-only variants, listed in the pin file); 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/dc_opentitan_usbdev_linkstate/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 1 Apache-2.0 files; 2 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/ip_opentitan_csrng/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 68 Apache-2.0 files (4 local lift-only variants, listed in the pin file); 4 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/ip_opentitan_edn/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 60 Apache-2.0 files (4 local lift-only variants, listed in the pin file); 4 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/m0_opentitan_prim_arbiter/source` | lowRISC/opentitan | `d71121d8452f` | Apache-2.0 | 7 Apache-2.0 files; 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/m1_opentitan_uart_tx/source` | lowRISC/opentitan | `21f062eb67c8` | Apache-2.0 | 1 Apache-2.0 files | `UPSTREAM_COMMIT.txt` |
| `examples/verify/m2_opentitan_csrng_main_sm/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 1 Apache-2.0 files; 2 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/m3_clustered_coi_csrng_pair/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 1 Apache-2.0 files; 3 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/opentitan_prim_onehot_check/source` | lowRISC/opentitan | `d71121d8452f` | Apache-2.0 | 6 Apache-2.0 files; 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/r46_pattgen_per_cluster/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 2 Apache-2.0 files; 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/r46_sysrst_detect_k5/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 2 Apache-2.0 files; 2 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/v10_opentitan_nested_count_recoverability/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 2 Apache-2.0 files; 3 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/v8_opentitan_fifo_recoverability/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 3 Apache-2.0 files; 1 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/v9_opentitan_count_recoverability/source` | lowRISC/opentitan | `558921ccc8ae` | Apache-2.0 | 2 Apache-2.0 files; 3 first-party (stubs, sidecars) | `UPSTREAM_COMMIT.txt` |
| `examples/verify/sv_yosys_caliptra_rtl_150/source` | chipsalliance/caliptra-rtl | not recorded | Apache-2.0 | 3 Apache-2.0 files (the `soc_ifc` boot FSM before and after caliptra-rtl #150, and its package); 4 first-party stubs | `UPSTREAM_COMMIT.txt` (commit unknown; says so) |

## Derived works of vendored RTL

| directory | derived from | licence | what it is | provenance |
|---|---|---|---|---|
| `examples/verify/v7b_uart_recoverability/source` | `examples/verify/m1_opentitan_uart_tx/source` (lowRISC/opentitan `21f062eb`) | Apache-2.0 | `uart_tx_stuck.sv`: upstream `uart_tx.sv` with one planted bug | `UPSTREAM_COMMIT.txt` |
| `examples/recoverability` | the `dc_opentitan_*` directories above (lowRISC/opentitan `558921cc`) | Apache-2.0 | `csrng_main_sm_fsm.sv`, `edn_boot_sm.sv`, `otbn_start_stop_fsm.sv`, `rom_ctrl_fsm.sv`: OpenTitan FSMs extracted for isolated verification, each header naming its source file; `compute_engine*.sv` are first-party (clean-room, see README) | the file headers |
| `crates/mununu-core/tests/fixtures/wall_classes` | the `dc_opentitan_*` directories and `examples/recoverability` above; one OpenCores core | Apache-2.0 (OpenTitan lifts); OpenCores notice (`i2c_scl_padoen.btor`) | Yosys BTOR2 lifts of RTL modules, used as class representatives by the wall-class matrix | `PROVENANCE.md` in the directory |
| `crates/mununu-core/tests/data` | lowRISC/ibex | Apache-2.0 | `ibex_register_file_fpga_16x4.btor2`: a Yosys lift of `rtl/ibex_register_file_fpga.sv`, the source line named on every BTOR2 line | the file's header |

## Other third-party material

| directory | upstream | commit | licence | contents | provenance |
|---|---|---|---|---|---|
| `examples/btor2/btor2tools_suite` | Boolector/btor2tools `examples/btorsim/` | `d33c73ff1d17` | MIT | 9 BTOR2 benchmarks, byte-for-byte | `PROVENANCE.md` (licence text quoted) |
| `examples/industrial/codesign_nrf52_twim_i2c/upstream` | NordicSemiconductor/nrfx | `0883a272c340` | BSD-3-Clause | `nrfx_twim.c` (verbatim), `nrfx_twim_buggy.c` (one planted bug), `nrf52840.svd` | `ATTRIBUTION.md`, `nrfx-LICENSE.txt` |

## Directories that look vendored and are not

`examples/verify/r46_gap2_wide_concretize`, `r46_gap2_per_cluster_concretize`, `r46_synth_two_cones`
and `v6_controllability_kmts` hold hand-written fixtures; their headers say so (`SYNTHETIC`,
`NOT vendored`). They carry no pin file because there is nothing to pin.

## Known gaps

- The caliptra-rtl commit was not recorded when the files were vendored; the pin file says so.
- `i2c_scl_padoen.btor` is a lift of an OpenCores core whose RTL is not in this repository; the
  lift does not reproduce the core's copyright notice. See the fixture directory's `PROVENANCE.md`.
- Pin files are not all in one format (some carry only the commit): roadmap item G2.

## Bundled in the release archives, not in this repository

| what | where | licence |
|---|---|---|
| `libz3` (the Z3 theorem prover's shared library: `libz3.so.4` in the Linux archives, `libz3.<version>.dylib` in the macOS archives, `libz3.dll` in the Windows zip) | copied from the runner's Z3 package by [`.github/workflows/release.yml`](.github/workflows/release.yml) and placed beside the binaries, which load it through `$ORIGIN` / `@executable_path` | MIT, © Microsoft Corporation — [`LICENSES/Z3-MIT.txt`](LICENSES/Z3-MIT.txt), also shipped in every archive's `LICENSES/` |

## Bundled in the on-prem image, not in this repository

> Source of truth: [`docker/Dockerfile.bundle`](docker/Dockerfile.bundle), [`scripts/bundle.sh`](scripts/bundle.sh) — surface: CLI-only — the image and tarball are built by a script, not served by the binary

The on-prem bundle (`scripts/bundle.sh`, [`deploy/README.md`](deploy/README.md)) is a Docker
image that carries more than the mununu binary. Everything in it beyond the first-party work stays
under its own licence. The image and the tarball carry the texts; [`deploy/NOTICE.md`](deploy/NOTICE.md)
says where each one is.

| what | in the image | licence | texts |
|---|---|---|---|
| mununu's own notices | `/usr/share/doc/mununu/` | the [Mununu Non-Commercial License](LICENSE); Z3 MIT | `LICENSE`, `THIRD_PARTY.md`, `LICENSES/`, `LICENSING.md` there and beside the tarball |
| oss-cad-suite, one pinned release (`OSS_CAD_SUITE_TAG` in the Dockerfile; the image label `io.github.mumunu-team.oss-cad-suite.tag`) | `/opt/oss-cad-suite/`, unmodified, as YosysHQ publishes it | per component: 57 texts. Most are permissive (yosys and the slang plugin ISC, cvc5 and pono BSD, btor2tools and Bitwuzla MIT). Several are copyleft: Verilator (LGPL-3 or Artistic-2), yices, rIC3 and the ghdl-yosys plugin (GPL-3), ghdl, iverilog and dfu-util (GPL-2), gtkwave and openocd (GPL), libpoly and xdot (LGPL-3), libusb (LGPL-2.1), libftdi (LGPL-2) | `/opt/oss-cad-suite/license/LICENSE.<component>`; the suite's own `share/manifest.json` maps files to components |
| Debian trixie-slim base and the packages the Dockerfile installs (`libz3-4`, `g++`, `make`, `perl`, `curl`, `ca-certificates`) | the system tree | per package (the GNU toolchain GPL-3 with the GCC runtime exception, Perl GPL or Artistic, Z3 MIT, curl's own licence, …) | `/usr/share/doc/<package>/copyright` |
| mununu-ui's static build and the npm packages compiled into it | `/srv/mununu-ui/` | mununu-ui's `LICENSE`; per npm package | `/srv/mununu-ui/LICENSE.txt` and `/srv/mununu-ui/THIRD_PARTY_NOTICES.txt` (generated at build time; `scripts/bundle.sh` refuses a UI build without them) |

The suite's components run as separate processes; mununu does not link them. Their sources are
published upstream (the YosysHQ release tag, each component's own repository). How a recipient of
the bundle obtains the corresponding source of the copyleft components is an open question for
counsel.
