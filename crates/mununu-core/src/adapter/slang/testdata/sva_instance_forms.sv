// Named sequence / property instances, a sequence `or` antecedent and elements of an unpacked PORT —
// the shapes `assertion_rewrite` lands in the fragment. Captured 2026-10-21 in the `mununu-sva` image
// (slang 11.0.448+e222e7dc0):
//   slang --quiet --single-unit --ast-json sva_instance_forms.ast.json --top forms sva_instance_forms.sv
module forms #(parameter int N = 8, parameter int DW = 4) (
  input  logic clk_i, rst_ni,
  input  logic ping_p, ping_n, ack_p, ack_n, alert_p, alert_n,
  input  logic ready_i, valid_o,
  input  logic [$clog2(N)-1:0] idx_o,
  input  logic [DW-1:0] data_i [N],
  output logic [DW-1:0] data_o
);
  sequence PingSigInt_S;
    ping_p == ping_n [*2];
  endsequence
  sequence AckSigInt_S;
    ack_p == ack_n [*2];
  endsequence
  // (i) a named sequence instance after ##1 in an antecedent
  SigIntPing_A: assert property (@(posedge clk_i) disable iff (!rst_ni)
      ##1 PingSigInt_S |-> ##[3:4] alert_p == alert_n);
  // (ii) an `or` of two sequence instances as the antecedent
  InBandInit_A: assert property (@(posedge clk_i) disable iff (!rst_ni)
      PingSigInt_S or AckSigInt_S |-> ##[3:4] alert_p == alert_n);
  // (iii) a variable-index unpacked-port element compare, and a constant one
  DataFlow_A: assert property (@(posedge clk_i) disable iff (!rst_ni)
      ready_i && valid_o |-> data_o == data_i[idx_o]);
  DataFlowK_A: assert property (@(posedge clk_i) disable iff (!rst_ni)
      ready_i && valid_o && idx_o == 3 |-> data_o == data_i[3]);
  // (iv) a named property with a local variable (the ASSERT_FPV_LINEAR_FSM shape)
  property Linear_P;
    logic [2:0] init_s;
    (!$stable(idx_o), init_s = $past(idx_o)) |-> (idx_o != init_s) until !ready_i;
  endproperty
  Linear_A: assert property (@(posedge clk_i) disable iff (!rst_ni) Linear_P);
  assign data_o = data_i[idx_o];
endmodule
