// 4-bit DMR (dual modular redundancy) commit gate — fault-tolerance contract example.
//
// Companion design note: docs/design/fault-tolerance-contracts.md
//
// Property under verification:
//   "Given a single-bit fault in replica A's output, the commit gate must
//    never broadcast a value that disagrees with the fault-free reference
//    (replica B)."
//
// Fault model (one-hot single-bit flip):
//   - When `flip_en` is asserted, exactly ONE bit of y_a is XORed with 1.
//     The bit index is the env-chosen `flip_idx` ∈ {0,1,2,3}.
//   - Encoded structurally as `y_a ^ (4'b0001 << flip_idx)`. The mask is
//     one-hot by construction — MBU is excluded by encoding, not by
//     assumption alone.
//
// What this DOES verify:
//   - Under every reachable (x, flip_idx, flip_en) input combination, the
//     comparator catches every one-hot fault and de-asserts commit_valid.
//   - When commit_valid is asserted, broadcast_data matches the registered
//     fault-free reference y_b_q.
//
// What this DOES NOT verify (see README caveats):
//   - Datapath correctness (replicas modelled as y = x).
//   - Faults that hit both replicas identically (correlated / common-mode
//     failure — the Meta/Google 2021 SDC failure mode).
//   - Multi-bit upsets, persistent faults, faults on the comparator or
//     commit register themselves.
//   - Liveness.
//
// Adapter-safety patterns used (see CLAUDE.md "Adapter / Emitter Capability Use"):
//   - All registered signals declared as INTERNAL `logic`, then `assign`-ed
//     to outputs. The SV Kripke builder only treats internal `logic` regs
//     as state; output-port `logic` is treated as combinational.
//   - `^` (XOR) is the natural primitive for the fault mask. Supported on
//     this branch (see parser.rs:parse_bitxor_expr and kripke.rs BinOp::BitXor).
//   - No `concat()` — sidesteps the kripke.rs:1551 LSB truncation.
//   - Explicit reset for every registered signal — sidesteps the
//     kripke.rs:1405 fall-back-to-false unsoundness.
//   - All datapath widths ≤ 4 bits — fits the kripke.rs:918 auto-abstract.
//
// State budget: commit_valid_r(1) + broadcast_valid_r(1) + broadcast_data_r(4)
//   + y_b_q(4) + corrupted_broadcast_r(1) = 11 bits ≈ 2048 states. Well under
//   the 2^18 cap at kripke.rs:207.
//
// @mununu mode kripke
// @mununu domain x: bounded_counter 0..15
// @mununu domain flip_idx: bounded_counter 0..3
// @mununu input x, flip_idx, flip_en
module dmr_top(
    input  logic       clk,
    input  logic       rst,
    input  logic [3:0] x,             // env-driven operand
    input  logic [1:0] flip_idx,      // env: which bit of y_a to flip (0..3)
    input  logic       flip_en,       // env: enable single-bit flip on y_a
    output logic       commit_valid,
    output logic       broadcast_valid,
    output logic [3:0] broadcast_data,
    output logic       corrupted_broadcast
);

    // Two replicas of the same 4-bit FU. Identity here; in a real FU each
    // would compute the same nontrivial function. Verification only proves
    // the gating layer.
    logic [3:0] y_a;
    logic [3:0] y_b;
    assign y_a = x;
    assign y_b = x;

    // Adversarial single-bit flip on replica A. The mask is `4'b0001 << flip_idx`,
    // structurally one-hot. When flip_en=0 the value passes through unchanged.
    logic [3:0] y_a_post;
    assign y_a_post = flip_en ? (y_a ^ (4'b0001 << flip_idx)) : y_a;

    // Comparator (combinational).
    logic equal;
    assign equal = (y_a_post == y_b);

    // Internal registers — the actual state. Output ports are pure shadows.
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
            commit_valid_r    <= equal;
            broadcast_valid_r <= equal;
            broadcast_data_r  <= y_a_post;
            y_b_q             <= y_b;
            // Observer: latches if a commit ever broadcasts a value that
            // disagrees with the pipelined golden reference. This is the
            // safety property expressed in observable form — translation
            // of the contract spec, not a "bug detector."
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
