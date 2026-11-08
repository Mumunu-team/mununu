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

    // The table is cleared at reset. Without an established initial state the tool withholds
    // a definite VIOLATED on the twin (a free cycle-0 table over-approximates reachability and
    // licenses only HOLDS), which is what the first measurement found; a power-on clear is what
    // the consumer's controller does anyway, and it leaves the capture-timing question intact.
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            for (int i = 0; i < LINES; i++) mem[i] <= '0;
        end else if (we) begin
            mem[waddr] <= wdata;
        end
    end

    assign mem_rd = mem[line];

    // THE PLANTED FAULT: the strobe is ignored; the capture happens every cycle.
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) ent_q <= '0;
        else        ent_q <= mem_rd;
    end

    assign enable = ent_q[0];
endmodule
