// line_table — the three-access shape of mununu#545 (roadmap F1), hand-written, NOT vendored.
//
// A table a controller loads, read continuously at a free index, sampled into a plain
// register once per strobe. The three accesses are distinct, and the distinction is the
// whole question of #545:
//
//   1. the array WRITE  — conditional, at its own index (`we`, `waddr`);
//   2. the array READ   — unconditional and continuous, at a FREE index (`line`) that may
//                         advance every cycle;
//   3. the CAPTURE      — a register downstream of the read, gated by `line_stb`, which is
//                         not a memory access at all.
//
// The properties care WHEN the captured entry changes, never WHICH value it holds. A
// cutpoint on the read frees the value and the timing together (the twin then refutes
// nothing); the exact engine refuses the array operator; the question is whether the array-aware
// native BMC / k-induction (N5) decides the capture-timing properties as written, or whether a
// value-only havoc at the read is needed (design work). See README.md.
module line_table #(
    parameter int LINES = 4,
    parameter int WIDTH = 4
) (
    input  logic                     clk,
    input  logic                     rst_n,
    input  logic                     we,
    input  logic [$clog2(LINES)-1:0] waddr,
    input  logic [WIDTH-1:0]         wdata,
    input  logic [$clog2(LINES)-1:0] line,
    input  logic                     line_stb,
    output logic                     enable
);
    logic [WIDTH-1:0] mem [LINES];
    logic [WIDTH-1:0] mem_rd;
    logic [WIDTH-1:0] ent_q;

    // 1. the write: conditional, own index.
    always_ff @(posedge clk) begin
        if (we) mem[waddr] <= wdata;
    end

    // 2. the read: unconditional, continuous, free index.
    assign mem_rd = mem[line];

    // 3. the capture: a plain register, gated by the strobe. This is what the properties
    //    are about.
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n)        ent_q <= '0;
        else if (line_stb) ent_q <= mem_rd;
    end

    assign enable = ent_q[0];
endmodule
