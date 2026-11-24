module top(input logic clk_a, input logic clk_b, input logic rst, input logic req,
           input logic [1:0] wa, input logic wd, output logic y);
  logic seen;
  logic m [4];
  always_ff @(posedge clk_a)
    if (rst) seen <= 1'b0;
    else seen <= req;
  always_ff @(posedge clk_b)
    if (seen) m[wa] <= wd;
  assign y = m[wa] & ~rst;
endmodule
