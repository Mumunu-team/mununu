module top(input logic clk_a, input logic clk_b, input logic rst, input logic req, input logic we,
           input logic [1:0] wa, output logic y);
  logic seen;
  logic m [4];
  always_ff @(posedge clk_a)
    if (rst) seen <= 1'b0;
    else seen <= req;
  always_ff @(posedge clk_b)
    if (we) m[wa] <= seen;
  assign y = m[wa] & ~rst;
endmodule
