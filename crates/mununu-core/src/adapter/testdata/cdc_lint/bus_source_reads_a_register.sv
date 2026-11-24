module top(input logic clk_a, input logic clk_b, input logic rst,
           output logic [1:0] o);
  logic [1:0] g, h, ptr, s1, s2;
  always_ff @(posedge clk_b)
    if (rst) g <= 2'b00;
    else g <= {g[0], ~g[1]};
  always_ff @(posedge clk_b)
    if (rst) h <= 2'b00;
    else h <= g;
  always_ff @(posedge clk_a)
    if (rst) ptr <= 2'b00;
    else ptr <= h;
  always_ff @(posedge clk_b) begin s1 <= ptr; s2 <= s1; end
  assign o = s2;
endmodule
