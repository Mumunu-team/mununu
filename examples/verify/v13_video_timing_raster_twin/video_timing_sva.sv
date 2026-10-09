// Tier-1 safety assertions for video_timing, bound in from outside.
//
// Every atom is `hcount_q` or `vcount_q` — the only registers in the block, and
// the only two things every downstream count derives from. There are no data
// inputs at all, which is why this block can own its tier 2 (see the design's
// annotations) and why nothing here needs an abstraction.
//
// The literals are H_TOTAL-1 = 799 and V_TOTAL-1 = 524. Property atoms take
// literals rather than localparams — a parameter name does not survive the lift
// — so the mode is written down a second time HERE and must move if it moves
// there. That coupling is the price of a decidable property and is stated so it
// is re-read rather than discovered.

module video_timing_sva (
    input logic       clk,
    input logic       rst_n,
    input logic [9:0] hcount_q,
    input logic [9:0] vcount_q
);

    // --- the line wraps exactly at the end -----------------------------------
    a_line_wraps_at_total: assert property (@(posedge clk) disable iff (!rst_n)
        (hcount_q == 10'd799) |=> (hcount_q == 10'd0));

    // --- the frame wraps exactly at the end ----------------------------------
    a_frame_wraps_at_total: assert property (@(posedge clk) disable iff (!rst_n)
        ((hcount_q == 10'd799) && (vcount_q == 10'd524)) |=> (vcount_q == 10'd0));

    // --- THE ROW MOVES ONLY AT A LINE BOUNDARY -------------------------------
    //
    // The one worth having. A `vcount_q` that advanced mid-line would tear every
    // frame — the top part of a scanline from one row and the rest from the
    // next — and it would do so IDENTICALLY in simulation, so a sim-versus-board
    // frame CRC could never see it. That is V-04a's MULTIPLICITY lesson in the
    // block every other count derives from.
    a_row_advances_only_at_line_end: assert property (@(posedge clk) disable iff (!rst_n)
        (hcount_q != 10'd799) |=> (vcount_q == $past(vcount_q)));

    // --- the counters stay inside the mode, stated INDUCTIVELY ---------------
    //
    // Rule 6 pins the reset inactive, so every register starts arbitrary and an
    // absolute bound is VIOLATED at depth 0 on correct RTL. Preservation is the
    // claim that holds from any state. Not tautologies: both counters are 10
    // bits (0..1023) against bounds of 799 and 524.
    a_hcount_bound_preserved: assert property (@(posedge clk) disable iff (!rst_n)
        (hcount_q <= 10'd799) |=> (hcount_q <= 10'd799));

    a_vcount_bound_preserved: assert property (@(posedge clk) disable iff (!rst_n)
        (vcount_q <= 10'd524) |=> (vcount_q <= 10'd524));

endmodule

bind video_timing video_timing_sva u_sva (.*);
