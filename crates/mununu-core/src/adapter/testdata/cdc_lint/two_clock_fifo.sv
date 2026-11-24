module top(input logic clk_a, input logic clk_b, input logic rst, input logic push, input logic pop,
           input logic wd, output logic q);
  logic m [4];
  logic [1:0] wptr, rptr;
  logic rd;
  always_ff @(posedge clk_a)
    if (rst) wptr <= '0;
    else if (push) begin m[wptr] <= wd; wptr <= wptr + 2'd1; end
  always_ff @(posedge clk_b)
    if (rst) rptr <= '0;
    else if (pop) rptr <= rptr + 2'd1;
  assign rd = m[rptr];
  always_ff @(posedge clk_b) q <= rd;
endmodule
