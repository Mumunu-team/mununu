// Hierarchical references into a module instantiated several times, written the way OpenTitan's
// `ASSERT_PRIM_*_ERROR_TRIGGER_ALERT` macros write them: the path arrives as a macro argument, so
// its text exists only after preprocessing. slang's JSON names the DECLARATION (`err_o`, shared by
// every `cnt`); the path text — sliced from the preprocessed unit via `--ast-json-source-info` —
// says which instance. Captured 2026-10-17 in the mununu-sva image (slang 11.0.448):
//   slang -E --single-unit sva_hierarchy_refs.sv > sva_hierarchy_refs.pp.sv
//   slang --quiet --single-unit --ast-json sva_hierarchy_refs.ast.json --ast-json-source-info \
//         sva_hierarchy_refs.pp.sv
// Consumed by `hierarchy_refs_resolve_from_real_slang_json` in ../translate.rs.
`define ERR_TO_ALERT(NAME_, HIER_, ALERT_) \
  NAME_: assert property (@(posedge clk) $rose(HIER_.err_o) |-> ##[0:2] (ALERT_));
module cnt (input logic clk, input logic inc, output logic err_o);
  logic [1:0] q;
  always_ff @(posedge clk) q <= q + inc;
  assign err_o = (q == 2'd3);
endmodule
module fifo (input logic clk, input logic inc, output logic wptr_err_o);
  if (1) begin : gen_normal
    cnt u_wptr (.clk, .inc, .err_o(wptr_err_o));
    cnt u_rptr (.clk, .inc(1'b0), .err_o());
  end
endmodule
module top (input logic clk, input logic inc, output logic alert_a, output logic alert_b);
  // The alert is wired through a port: RTL never reads `u_a.….err_o` hierarchically (yosys would
  // leave such a read an undriven wire — a free input); only the assertions do.
  fifo u_a (.clk, .inc, .wptr_err_o(alert_a));
  fifo u_b (.clk, .inc, .wptr_err_o());
  logic [1:0] ep_err;
  for (genvar i = 0; i < 2; i++) begin : gen_ep
    cnt u_ep (.clk, .inc(inc), .err_o(ep_err[i]));
  end
  assign alert_b = 1'b0;
  `ERR_TO_ALERT(a_wptr_alerts, u_a.gen_normal.u_wptr, alert_a)
  `ERR_TO_ALERT(b_wptr_silent, u_b.gen_normal.u_wptr, alert_b)
  `ERR_TO_ALERT(a_rptr_never, u_a.gen_normal.u_rptr, alert_b)
  for (genvar i = 0; i < 2; i++) begin : gen_ep_chk
    `ERR_TO_ALERT(ep_alerts, gen_ep[i].u_ep, ep_err[i])
  end
endmodule
