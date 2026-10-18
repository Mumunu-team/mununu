
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
  
  
  fifo u_a (.clk, .inc, .wptr_err_o(alert_a));
  fifo u_b (.clk, .inc, .wptr_err_o());
  logic [1:0] ep_err;
  for (genvar i = 0; i < 2; i++) begin : gen_ep
    cnt u_ep (.clk, .inc(inc), .err_o(ep_err[i]));
  end
  assign alert_b = 1'b0;
  
  a_wptr_alerts: assert property (@(posedge clk) $rose(u_a.gen_normal.u_wptr.err_o) |-> ##[0:2] (alert_a));
  
  b_wptr_silent: assert property (@(posedge clk) $rose(u_b.gen_normal.u_wptr.err_o) |-> ##[0:2] (alert_b));
  
  a_rptr_never: assert property (@(posedge clk) $rose(u_a.gen_normal.u_rptr.err_o) |-> ##[0:2] (alert_b));
  for (genvar i = 0; i < 2; i++) begin : gen_ep_chk
    
  ep_alerts: assert property (@(posedge clk) $rose(gen_ep[i].u_ep.err_o) |-> ##[0:2] (ep_err[i]));
  end
endmodule

