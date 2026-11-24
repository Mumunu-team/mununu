module top(input logic clk_a, input logic clk_b, input logic d, input logic we,
           output logic o, output logic y);
  logic a, s1, s2, t;
  always_ff @(posedge clk_a) a <= d;
  always_ff @(posedge clk_b) begin s1 <= a; s2 <= s1; end
  always_ff @(posedge clk_b) if (we) t <= s1;
  assign o = s2;
  assign y = t;
endmodule
