module top(input logic clk_a, input logic clk_b, input logic wea, input logic web,
           input logic [1:0] waa, input logic [1:0] wab, input logic wda, input logic wdb,
           input logic [1:0] ra, output logic y);
  logic m [4];
  always_ff @(posedge clk_a)
    if (wea) m[waa] <= wda;
  always_ff @(posedge clk_b)
    if (web) m[wab] <= wdb;
  assign y = m[ra];
endmodule
