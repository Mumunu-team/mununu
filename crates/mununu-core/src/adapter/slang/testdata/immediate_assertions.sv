// mununu#988 / mununu#1001 — immediate assert / assume / cover the run does not check: one
// in each procedural form, two instances of a module holding one, and a generate branch the
// elaboration does not take. Captured 2026-11-12 in the mununu-sva image (slang 11.0.448):
//   slang --top m --ast-json immediate_assertions.ast.json --ast-json-source-info immediate_assertions.sv
// Consumed by `every_immediate_assertion_is_recorded_once_per_instance` in ../translate.rs.
module sub(input logic clk, input logic [1:0] x);
  always_ff @(posedge clk) assert (x != 2'b10);
endmodule

module m #(parameter bit EXTRA = 0) (input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) begin
    q <= d;
    assume (d != 2'b11);
  end
  always_comb begin
    a_imm: assert (q != 2'b10);
    cover (q == 2'b01);
  end
  sub u_a (.clk(clk), .x(d));
  sub u_b (.clk(clk), .x(q));
  if (EXTRA) begin : g_extra
    always_comb assert (q != 2'b00);
  end
  ap: assert property (@(posedge clk) q != 2'b11);
endmodule
