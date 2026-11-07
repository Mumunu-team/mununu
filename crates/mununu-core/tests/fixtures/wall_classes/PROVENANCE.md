# Wall-class fixtures — provenance

These `.btor` files are Yosys BTOR2 lifts of RTL modules (each file's first line names the module
and the Yosys build). A lift of licensed RTL is a derived work of it, so each fixture is under the
licence of the RTL it was lifted from. The RTL itself is vendored and pinned elsewhere in this
repository; the matrix (`tests/wall_class_matrix.rs`) names each case's reproduction path.

| fixture | lifted from | pinned in | licence |
|---|---|---|---|
| `aes_cipher_control_fsm.btor` | OpenTitan `aes_cipher_control_fsm` | `examples/verify/dc_opentitan_aes_cipher_control_fsm/source` (lowRISC/opentitan `558921cc`) | Apache-2.0 |
| `aes_ctr_fsm.btor` | OpenTitan `aes_ctr_fsm` | `examples/verify/dc_opentitan_aes_ctr_fsm/source` (`558921cc`) | Apache-2.0 |
| `csrng_main_sm.btor` | OpenTitan `csrng_main_sm` | `examples/verify/dc_opentitan_csrng_main_sm/source` (`558921cc`) | Apache-2.0 |
| `csrng_main_sm_fsm.btor` | `examples/recoverability/csrng_main_sm_fsm.sv`, an extract of OpenTitan `csrng_main_sm` | as above | Apache-2.0 |
| `edn_boot_sm.btor` | `examples/recoverability/edn_boot_sm.sv`, an extract of OpenTitan `edn_main_sm` | `examples/verify/dc_opentitan_edn_main_sm/source` (`558921cc`) | Apache-2.0 |
| `otbn_start_stop_fsm.btor` | OpenTitan `otbn_start_stop_control` (`examples/recoverability/otbn_start_stop_fsm.sv`) | `examples/verify/dc_opentitan_otbn_start_stop_control/source` (`558921cc`) | Apache-2.0 |
| `rom_ctrl_fsm.btor` | OpenTitan `rom_ctrl_fsm` (`examples/recoverability/rom_ctrl_fsm.sv`) | `examples/verify/dc_opentitan_rom_ctrl_fsm/source` (`558921cc`) | Apache-2.0 |
| `prim_arbiter_tree_n8.btor` | OpenTitan `prim_arbiter_tree` (N = 8) | `examples/verify/dc_opentitan_prim_arbiter_tree/source` (`558921cc`) | Apache-2.0 |
| `i2c_scl_padoen.btor` | OpenCores I²C master (`i2c_master_top`) | **not in this repository**: the RTL was lifted from a local copy and is not vendored | the OpenCores core's own notice; **not reproduced in the lift** (gap, see below) |

The Apache-2.0 text is [`LICENSES/Apache-2.0.txt`](../../../../../LICENSES/Apache-2.0.txt) at the
repository root; the root `THIRD_PARTY.md` lists every vendored directory.

**Gap.** `i2c_scl_padoen.btor` carries only Yosys's banner. The OpenCores core it was lifted from
distributes its RTL under a notice that asks for the copyright statement to be kept in derived
work. Whoever re-lifts this fixture, or vendors the core, adds that notice to the file's header
and to this table.
