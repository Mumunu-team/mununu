module top(input logic clk_a, input logic clk_b, input logic d, input logic we,
           input logic [1:0] wa, output logic o, output logic y);
  logic a, s1, s2;
  logic m [4];
  always_ff @(posedge clk_a) a <= d;
  always_ff @(posedge clk_b) begin s1 <= a; s2 <= s1; end
  always_ff @(posedge clk_b) if (we) m[wa] <= s1;
  assign o = s2;
  assign y = m[wa];
endmodule
