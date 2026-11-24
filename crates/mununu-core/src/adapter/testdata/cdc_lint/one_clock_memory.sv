module top(input logic clk, input logic we, input logic [1:0] wa, input logic [1:0] ra,
           input logic wd, output logic q);
  logic m [4];
  always_ff @(posedge clk)
    if (we) m[wa] <= wd;
  always_ff @(posedge clk)
    q <= m[ra];
endmodule
