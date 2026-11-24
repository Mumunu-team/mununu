module ram(input logic clk, input logic we, input logic [1:0] wa, input logic [1:0] ra,
           input logic wd, output logic rd);
  logic m [4];
  always_ff @(posedge clk) if (we) m[wa] <= wd;
  assign rd = m[ra];
endmodule
module top(input logic clk_a, input logic clk_b, input logic we,
           input logic [1:0] wa, input logic [1:0] ra, input logic wd, output logic q);
  logic rd;
  ram u_ram(.clk(clk_a), .we(we), .wa(wa), .ra(ra), .wd(wd), .rd(rd));
  always_ff @(posedge clk_b) q <= rd;
endmodule
