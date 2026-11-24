module top(input logic clk_a, input logic clk_b, input logic rst, input logic sel,
           output logic [1:0] o);
  logic [1:0] m [2];
  logic [1:0] g, ptr, s1, s2;
  always_ff @(posedge clk_b)
    if (rst) g <= 2'b00;
    else g <= {g[0], ~g[1]};
  always_ff @(posedge clk_b)
    if (rst) begin m[0] <= 2'b00; m[1] <= 2'b00; end
    else begin m[sel] <= g; m[~sel] <= g; end
  always_ff @(posedge clk_a)
    if (rst) ptr <= 2'b00;
    else ptr <= m[sel];
  always_ff @(posedge clk_b) begin s1 <= ptr; s2 <= s1; end
  assign o = s2;
endmodule
