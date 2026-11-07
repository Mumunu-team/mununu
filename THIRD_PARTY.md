# Third-party files in this repository

> Scope: every directory below holds files that are the work of others, or derived from it.
> They are **not** under the [Mununu Non-Commercial License](LICENSE); each stays under the
> licence named in its row, whose text is in [`LICENSES/`](LICENSES/) or beside the files. Everything
> not listed here is first-party and under `LICENSE`. The test
> `crates/mununu-core/tests/third_party_inventory_g1.rs` fails when a directory with a
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
