// Where names and assertions live: the shapes `SourceIndex` reads (2026-10-30). Captured in the
// `mununu-sva` image (slang 11.0.448+e222e7dc0):
//   slang --quiet --ast-json source_index_forms.ast.json --ast-json-source-info --top top source_index_forms.sv
module leaf (input logic clk, input logic d, output logic q);
  always @(posedge clk) q <= d;
endmodule

module top (
  input  logic       clk,
  input  logic       rst_n,
  input  logic       go,
  output logic [1:0] state_q
);
  logic [1:0] state_d;
  logic [3:0] bits;
  logic       a, b;
  logic       done;

  always_comb begin
    state_d = state_q;
    if (go) state_d = state_q + 2'd1;
  end

  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) state_q <= '0;
    else        state_q <= state_d;
  end

  always_ff @(posedge clk) begin
    bits[state_q] <= go;
    {a, b} <= {b, a};
  end

  assign done = (state_q == 2'd3);

  for (genvar k = 0; k < 2; k++) begin : gen
    leaf u_leaf (.clk(clk), .d(bits[k]), .q());
  end

  a_wraps: assert property (@(posedge clk) disable iff (!rst_n) done |=> state_q == 2'd0);
  assert property (@(posedge clk) disable iff (!rst_n) state_q != 2'd2 || go);
endmodule
