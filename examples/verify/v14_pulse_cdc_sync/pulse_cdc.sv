// pulse_cdc — one pulse in, one pulse out, across a clock domain. Card V-14.
//
// ===========================================================================
// WHY IT EXISTS, AND IT IS A BUG THIS REPOSITORY PAID FOR ON HARDWARE
// ===========================================================================
// `vpu_regs` drives `blit_push` in the PIXEL domain at 25 MHz and `blit_fifo`
// runs on `clk_sys` at 37.5. A pulse one pixel-clock wide is **1.5 `clk_sys`
// cycles** wide, so the faster clock sampled it on TWO consecutive edges and
// every blit command was enqueued and executed twice — measured 2026-10-03,
// 502 pulses producing 1,004 pushes.
//
// **AND `make cdc-check` PASSED THROUGHOUT, because the crossing WAS declared.**
// Declaring a crossing says somebody thought about it; it does not say the pulse
// survives it. That is the general lesson and it is why this block exists rather
// than another inline fix.
//
// ===========================================================================
// A TOGGLE, NOT A LEVEL — and this is the same mechanism `bundle_cdc` uses
// ===========================================================================
// A pulse crossing into a SLOWER domain is lost; into a FASTER one it is seen
// several times. A level sampled by either is wrong in one of those two ways.
// A toggle is correct at ANY clock ratio: the source flips a bit per pulse, the
// destination resolves it in two flops and takes its EDGE, which yields exactly
// one far-side pulse per source event.
//
// `bundle_cdc` is this same toggle-and-edge with a payload latched beside it.
// The two are deliberately separate rather than one parameterised block: a pulse
// crossing has no data path, and a `W = 0` bundle would be a payload register
// that exists to be ignored. They share a mechanism, not a derived value.
//
// **THE OBLIGATION, which this block cannot check for itself**, and which is
// therefore declared in `rosf.toml` rather than asserted here: `pulse_in` must be
// rarer than the destination's synchroniser, which wants three destination
// cycles. Two pulses inside that window flip the toggle twice and the
// destination sees NO edge — it misses them both. `bundle_cdc` carries the same
// obligation for the same reason, and `video_sdram`'s use satisfies it by five
// orders of magnitude.
module pulse_cdc (
    // --- the source domain ---------------------------------------------------
    input  logic clk_src,
    input  logic rst_src_n,
    // One pulse per event. See the obligation above: rarer than three
    // destination cycles, or pulses are missed in PAIRS.
    input  logic pulse_in,

    // --- the destination domain ----------------------------------------------
    input  logic clk_dst,
    // The destination's OWN reset. With two PLLs a single reset's release edge
    // crosses as an ordinary signal, which is the one thing no crossing protocol
    // protects against — `sprite_stage` carries the argument.
    input  logic rst_dst_n,
    output logic pulse_out
);
    // --- source side: flip the toggle ---------------------------------------
    logic tog_q;
    always_ff @(posedge clk_src or negedge rst_src_n) begin
        if (!rst_src_n) tog_q <= 1'b0;
        else if (pulse_in) tog_q <= ~tog_q;
    end

    // --- destination side: two flops, then the edge ---------------------------
    //
    // The synchroniser has no reset — its job is to resolve metastability, and
    // resetting it buys a deterministic first cycle at the cost of a reset net in
    // a domain that has one anyway. The EDGE register does have one, because its
    // reset value decides whether a spurious pulse happens at release.
    logic meta_q, sync_q, sync_d_q;
    always_ff @(posedge clk_dst) begin
        meta_q <= tog_q;
        sync_q <= meta_q;
    end
    always_ff @(posedge clk_dst or negedge rst_dst_n) begin
        if (!rst_dst_n) sync_d_q <= 1'b0;
        else            sync_d_q <= sync_q;
    end

    assign pulse_out = sync_q ^ sync_d_q;

endmodule
