// line_table_twin — the contrast twin of line_table: the capture ignores the strobe.
//
// One line differs from line_table.sv: the entry register samples the read EVERY cycle, so
// `enable` can change between strobes. Both capture-timing properties must be VIOLATED here;
// an abstraction under which they hold on this twin has removed the timing it claims to check
// (that is what `--cutpoint` on the read did in mununu#545).
module line_table_twin #(
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

    // The table's power-on value is an `initial` block, not a reset, on purpose. Run 1 (no init,
    // no reset) left the twin's VIOLATED withheld as unestablished-initial-state: a free cycle-0
    // table licenses HOLDS, never a definite VIOLATED. Run 2 cleared the table under the async
    // reset, and yosys turned the memory into four registers (0 array sorts in the BTOR2; the
    // exact engine decided it) — the array path, which is the whole question of #545, was never
    // exercised. An `initial` clear keeps the array (`$meminit`, one uniform word, which the BTOR2
    // backend writes as an `init` of the array sort) and gives it a known cycle-0 value.
    initial begin
        for (int i = 0; i < LINES; i++) mem[i] = '0;
    end
    always_ff @(posedge clk) begin
        if (we) mem[waddr] <= wdata;
    end

    assign mem_rd = mem[line];

    // THE PLANTED FAULT: the strobe is ignored; the capture happens every cycle.
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) ent_q <= '0;
        else        ent_q <= mem_rd;
    end

    assign enable = ent_q[0];
endmodule
