module top(input logic clk_a, input logic clk_b, input logic we,
           input logic [1:0] wa, input logic [1:0] ra, input logic wd, output logic q);
  logic m [4];
  always_ff @(posedge clk_a)
    if (we) m[wa] <= wd;
  always_ff @(posedge clk_b)
    q <= m[ra];
endmodule
