// Stub `prim_assert.sv` for sv2v + Yosys ingestion of OpenTitan RTL.
//
// All SVA macros expand to empty — synthesis-equivalent to
// OpenTitan's `prim_assert_dummy_macros.svh` (Apache 2.0).
//
// Also defines `PRIM_FLOP_SPARSE_FSM` as a plain `always_ff` register
// (the upstream definition wraps a `prim_sparse_fsm_flop` submodule
// for FPV / runtime-encoding hardening, neither of which matters for
// the synthesised KMTS lift). The plain `always_ff` form preserves
// the FSM's functional semantics — `state_q` updates to `state_d`
// each clock unless reset is asserted, then `state_q` becomes the
// reset value.
//
// SOUNDNESS: dropping the sparse-FSM hardening removes only the
// runtime "alert if state_q is not one of the legal encodings"
// behaviour; the property the M.2 milestone verifies (idle is
// reachable from every reachable state) doesn't depend on the
// hardening — the FSM transitions remain identical.

// INC_ASSERT is defined (2026-10-22): the RTL's assertion-SUPPORT logic (prim_sparse_fsm_flop's
// `unused_err_o`, prim_count's `fpv_err_present`) is in the lift; the assertion statements still
// expand to nothing. Without it the sec_cm fault flags are constant 0 and their checks are vacuous.
`define INC_ASSERT
`define ASSERT_I(__name, __prop)
`define ASSERT_INIT(__name, __prop)
`define ASSERT_INIT_NET(__name, __prop)
`define ASSERT_FINAL(__name, __prop)
`define ASSERT_AT_RESET(__name, __prop, __rst = 0)
`define ASSERT_AT_RESET_AND_FINAL(__name, __prop, __rst = 0)
`define ASSERT(__name, __prop, __clk = 0, __rst = 0)
`define ASSERT_NEVER(__name, __prop, __clk = 0, __rst = 0)
`define ASSERT_KNOWN(__name, __sig, __clk = 0, __rst = 0)
`define COVER(__name, __prop, __clk = 0, __rst = 0)
`define ASSUME(__name, __prop, __clk = 0, __rst = 0)
`define ASSUME_I(__name, __prop)
`define ASSERT_PULSE(__name, __prop, __clk = 0, __rst = 0)
`define ASSERT_IF(__name, __prop, __enable, __clk = 0, __rst = 0)
`define ASSERT_KNOWN_IF(__name, __sig, __enable, __clk = 0, __rst = 0)
// Keeps the macro's `<name>_cond` flop (synchronous form; reads only clk and rst, drives nothing)
// and drops its local-variable property, which the translator expands from the real macro.
`define ASSERT_FPV_LINEAR_FSM(__name, __sig, __type, __clk = clk_i, __rst = !rst_ni)       \
  (* keep *) logic __name``_cond;                                                           \
  always_ff @(posedge __clk) begin                                                          \
    if (__rst) __name``_cond <= 1'b0; else __name``_cond <= 1'b1;                           \
  end

`define PRIM_FLOP_SPARSE_FSM(__name, __d, __q, __type, __resval = '0, __clk = clk_i, __rst_n = rst_ni, __alert_trigger_sva_en = 1) \
  always_ff @(posedge __clk or negedge __rst_n) begin                                                                              \
    if (!__rst_n) begin                                                                                                            \
      __q <= __resval;                                                                                                             \
    end else begin                                                                                                                 \
      __q <= __d;                                                                                                                  \
    end                                                                                                                            \
  end
