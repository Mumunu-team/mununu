// video_timing_early_row — contrast twin (CLAUDE.md rule 5).
//
// Keeps the module NAME so the real assertion file binds unchanged.
// REGENERATE if the design's @mununu_guarantee annotations change: the SVA
// cannot drift, but the annotations are copied here and did once.
module video_timing #(
    parameter int H_ACTIVE = 640,
    parameter int H_FRONT  = 16,
    parameter int H_SYNC   = 96,
    parameter int H_BACK   = 48,
    parameter int V_ACTIVE = 480,
    parameter int V_FRONT  = 10,
    parameter int V_SYNC   = 2,
    parameter int V_BACK   = 33,
    // Active-low sync for 640x480p60. Flip if a display refuses to lock.
    parameter bit H_POLARITY = 1'b0,
    parameter bit V_POLARITY = 1'b0
) (
    input  logic       clk,      // pixel clock
    input  logic       rst_n,
    output logic       hsync,
    output logic       vsync,
    output logic       de,       // high during active video
    output logic [9:0] x,        // 0..H_ACTIVE-1 while de
    output logic [9:0] y         // 0..V_ACTIVE-1 while de
);

    localparam int H_TOTAL = H_ACTIVE + H_FRONT + H_SYNC + H_BACK;  // 800
    localparam int V_TOTAL = V_ACTIVE + V_FRONT + V_SYNC + V_BACK;  // 525

    localparam int H_SYNC_START = H_ACTIVE + H_FRONT;               // 656
    localparam int H_SYNC_END   = H_SYNC_START + H_SYNC;            // 752
    localparam int V_SYNC_START = V_ACTIVE + V_FRONT;               // 490
    localparam int V_SYNC_END   = V_SYNC_START + V_SYNC;            // 492

    logic [9:0] hcount_q, vcount_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            hcount_q <= '0;
            vcount_q <= '0;
        // THE DEFECT: the row advances one pixel EARLY, and NOTHING ELSE
        // changes — `hcount_q` still wraps at H_TOTAL-1 exactly as before.
        //
        // The first version of this twin moved the wrap condition itself, which
        // broke the line wrap and the counter bound as well. A twin that breaks
        // four properties does not demonstrate that any ONE of them is checking
        // anything; rule 5 wants the OPPOSITE verdict on a named property, not a
        // design that falls over. Surgical is the point.
        //
        // Chosen for how it LOOKS: the picture is still a picture, still 60 Hz,
        // still in sync. One column of every scanline comes from the next row —
        // a ragged right edge, the kind of thing a person blames on a capture
        // card, and something a sim-versus-board frame CRC cannot see at all
        // because the model has the fault too.
        end else if (hcount_q == 10'(H_TOTAL - 1)) begin
            hcount_q <= '0;
        end else if (hcount_q == 10'(H_TOTAL - 2)) begin
            hcount_q <= hcount_q + 10'd1;
            vcount_q <= (vcount_q == 10'(V_TOTAL - 1)) ? 10'd0 : vcount_q + 10'd1;
        end else begin
            hcount_q <= hcount_q + 10'd1;
        end
    end

    // Combinational off the counters: the TMDS encoder registers everything one
    // stage later, so sync and data stay aligned through the pipeline.
    assign de = (hcount_q < 10'(H_ACTIVE)) && (vcount_q < 10'(V_ACTIVE));
    assign x  = hcount_q;
    assign y  = vcount_q;

    assign hsync = ((hcount_q >= 10'(H_SYNC_START)) && (hcount_q < 10'(H_SYNC_END)))
                 ? H_POLARITY : ~H_POLARITY;
    assign vsync = ((vcount_q >= 10'(V_SYNC_START)) && (vcount_q < 10'(V_SYNC_END)))
                 ? V_POLARITY : ~V_POLARITY;


    // Tier-1 assertions are in video_timing_sva.sv, bound from outside.
    //
    // --- tier 2: response liveness, DUT-only ---------------------------------
    // The second block in this programme to own its tier 2, after uart_tx, and
    // for the same reason: there is no input anywhere in the counters' path.
    // Both advance unconditionally on every clock, so a free environment cannot
    // stop them. Every line ends.
    // @mununu_guarantee nu X. (((hcount_q == 0) || (mu Y. ((hcount_q == 0) || ([] Y)))) && [] X)
    //
    // --- tier 3: recoverability ----------------------------------------------
    // And the start of a line is reachable from every state — including the
    // states out of reset that the mode never actually visits, which is what
    // rule 6's reset pinning explores.
    // @mununu_guarantee nu Y.((mu X.((hcount_q == 0) || <> X)) && [] Y)
    //
    // Non-vacuity witness (rule 4): the counter genuinely leaves zero. Stated as
    // `!= 0` rather than `== 799`, because a witness pinned to the mode's exact
    // total is one a contrast twin that changes the total would break — and a
    // failing witness is indistinguishable from a vacuous property. That was
    // measured on uart_tx, not guessed.
    // @mununu_guarantee mu Z.((hcount_q != 0) || <> Z)

endmodule
