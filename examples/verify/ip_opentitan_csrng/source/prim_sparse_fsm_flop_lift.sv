// LOCAL (mununu): lift-only variant of OpenTitan prim_sparse_fsm_flop.sv — every `ifdef INC_ASSERT` region
// removed (lines 42-56, 61-65 of the vendored file; an `else` branch is kept). Those regions hold
// assertion-side logic the lift cannot compile (`sequence` blocks, the enum-method loop of
// `is_undefined_state`); every other line is byte-identical. Extraction reads the vendored
// file; this one replaces it for the lift only (`--lift-override prim_sparse_fsm_flop.sv=prim_sparse_fsm_flop_lift.sv`).
// Copyright lowRISC contributors (OpenTitan project).
// Licensed under the Apache License, Version 2.0, see LICENSE for details.
// SPDX-License-Identifier: Apache-2.0

`include "prim_assert.sv"

module prim_sparse_fsm_flop #(
  parameter int               Width      = 1,
  parameter type              StateEnumT = logic [Width-1:0],
  parameter logic [Width-1:0] ResetValue = '0,
  // This should only be disabled in special circumstances, for example
  // in non-comportable IPs where an error does not trigger an alert.
  parameter bit               EnableAlertTriggerSVA = 1
`ifdef SIMULATION
  ,
  // In case this parameter is set to a non-empty string, the
  // prim_sparse_fsm_flop_if will also force the signal with this name
  // in the parent module that instantiates prim_sparse_fsm_flop.
  parameter string            CustomForceName = ""
`endif
) (
  input             clk_i,
  input             rst_ni,
  input  StateEnumT state_i,
  output StateEnumT state_o
);

  logic unused_err_o;

  logic [Width-1:0] state_raw;
  prim_flop #(
    .Width(Width),
    .ResetValue(ResetValue)
  ) u_state_flop (
    .clk_i,
    .rst_ni,
    .d_i(state_i),
    .q_o(state_raw)
  );
  assign state_o = StateEnumT'(state_raw);

    assign unused_err_o = 1'b0;

  // If ASSERT_PRIM_FSM_ERROR_TRIGGER_ALERT is declared, the unused_assert_connected signal will
  // be set to 1 and the below check will pass.
  // If the assertion is not declared however, the statement below will fail.

endmodule
