// lane_arbiter — the trace-slice fixture (2026-10-30): a small sequencer with every statement
// shape the slice has to walk through. Two lanes are granted one at a time; a request from IDLE
// grants a lane, a release returns to IDLE, and STALL_LIMIT consecutive cycles without an ack while
// a lane is granted raise FAULT, which holds until a release. This is the FIRST DRAFT: it has no
// memory of the last lane, so a request from IDLE always grants A, and the alternation guarantee
// below is violated (request, release, request — both grants on A). The slice of that
// counterexample must name the IDLE arm (`next = LANE_A`) and the `start_a` pulse, and must NOT
// name the FAULT / default arms, the `start_b` pulse or the `assign`s, all of which the cone of
// influence (`involves:`) lists. Every value here is invented.
//
// @mununu_guarantee(alternates) ltl G (start_a -> X (!start_a W start_b))
module lane_arbiter #(
    parameter int STALL_LIMIT = 5
) (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       req,       // grant a lane
    input  logic       rel,       // release the lane
    input  logic       ack,       // the granted lane acknowledges
    output logic       grant_a,
    output logic       grant_b,
    output logic       busy,
    output logic       fault,
    output logic       start_a,   // one-cycle pulse: lane A has just been granted
    output logic       start_b,   // one-cycle pulse: lane B has just been granted
    output logic [1:0] state_q    // IDLE = 0, LANE_A = 1, LANE_B = 2, FAULT = 3
);
    localparam logic [1:0] IDLE = 2'd0, LANE_A = 2'd1, LANE_B = 2'd2, FAULT = 2'd3;
    logic [2:0] stall_q;    // consecutive un-acked cycles while a lane is granted
    logic [1:0] next;

    always_comb begin
        next = state_q;
        case (state_q)
            IDLE:           if (req) next = LANE_A;   // the first draft always grants A
            LANE_A, LANE_B: begin
                if (!ack && stall_q == 3'(STALL_LIMIT - 1)) next = FAULT;   // the fault wins over a release
                else if (rel)                             next = IDLE;
            end
            FAULT:          if (rel) next = IDLE;
            default:        next = FAULT;                                   // an illegal encoding fails safe
        endcase
    end

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            state_q <= IDLE;
            stall_q <= '0;
            start_a <= 1'b0;
            start_b <= 1'b0;
        end else begin
            state_q <= next;
            start_a <= (next == LANE_A) && (state_q != LANE_A);
            start_b <= (next == LANE_B) && (state_q != LANE_B);
            if ((state_q == LANE_A || state_q == LANE_B) && !ack) stall_q <= stall_q + 3'd1;
            else                                                  stall_q <= '0;
        end
    end

    assign grant_a = (state_q == LANE_A);
    assign grant_b = (state_q == LANE_B);
    assign busy    = grant_a || grant_b;
    assign fault   = (state_q == FAULT);
endmodule
