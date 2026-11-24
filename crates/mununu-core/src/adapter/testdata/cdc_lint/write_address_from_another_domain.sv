module top(input logic clk_a, input logic clk_b, input logic rst, input logic [1:0] req, input logic we,
           input logic wd, input logic [1:0] ra, output logic y);
  logic [1:0] seen;
  logic m [4];
  always_ff @(posedge clk_a)
    if (rst) seen <= '0;
    else seen <= req;
  always_ff @(posedge clk_b)
    if (we) m[seen] <= wd;
  assign y = m[ra] & ~rst;
endmodule
