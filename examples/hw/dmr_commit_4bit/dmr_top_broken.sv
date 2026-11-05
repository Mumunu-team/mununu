// 4-bit DMR commit gate — NEGATIVE CONTROL (intentionally broken).
//
// Companion: dmr_top.sv (correct).
//
// The break: commit_valid_r is asserted unconditionally instead of being
// gated by `equal`. This simulates the design without the DMR comparator
// in the commit path — exactly the failure mode the contract is meant to
// detect.
//
// Expected verdict for property `no_corrupt_broadcast`:
//   UNSAT — the observer latches as soon as flip_en=1 fires a one-hot
//   fault and the commit register propagates the corrupted value. This
//   demonstrates the formal verdict is meaningful: removing the mitigation
//   produces a counterexample, so the SAT verdict on the intact design
//   (dmr_top.sv) carries proof content.
//
// @mununu mode kripke
// @mununu domain x: bounded_counter 0..15
// @mununu domain flip_idx: bounded_counter 0..3
// @mununu input x, flip_idx, flip_en
module dmr_top_broken(
    input  logic       clk,
    input  logic       rst,
    input  logic [3:0] x,
    input  logic [1:0] flip_idx,
    input  logic       flip_en,
    output logic       commit_valid,
    output logic       broadcast_valid,
    output logic [3:0] broadcast_data,
    output logic       corrupted_broadcast
);

    logic [3:0] y_a;
    logic [3:0] y_b;
    assign y_a = x;
    assign y_b = x;

    logic [3:0] y_a_post;
    assign y_a_post = flip_en ? (y_a ^ (4'b0001 << flip_idx)) : y_a;

    // BUG: comparator is structurally present but its output is ignored.
    logic equal;
    assign equal = (y_a_post == y_b);

    logic       commit_valid_r;
    logic       broadcast_valid_r;
    logic [3:0] broadcast_data_r;
    logic [3:0] y_b_q;
    logic       corrupted_broadcast_r;

    always_ff @(posedge clk or posedge rst) begin
        if (rst) begin
            commit_valid_r        <= 1'b0;
            broadcast_valid_r     <= 1'b0;
            broadcast_data_r      <= 4'b0000;
            y_b_q                 <= 4'b0000;
            corrupted_broadcast_r <= 1'b0;
        end else begin
            // BUG: commit fires unconditionally, ignoring `equal`.
            commit_valid_r    <= 1'b1;
            broadcast_valid_r <= 1'b1;
            broadcast_data_r  <= y_a_post;
            y_b_q             <= y_b;
            if (commit_valid_r && (broadcast_data_r != y_b_q)) begin
                corrupted_broadcast_r <= 1'b1;
            end
        end
    end

    assign commit_valid        = commit_valid_r;
    assign broadcast_valid     = broadcast_valid_r;
    assign broadcast_data      = broadcast_data_r;
    assign corrupted_broadcast = corrupted_broadcast_r;

endmodule
