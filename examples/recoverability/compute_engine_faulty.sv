// compute_engine_faulty.sv — the RECOVERABILITY-BUG contrast to compute_engine.sv.
//
// Authored for mununu as a PUBLIC, permissively-licensed (this repo's license) demonstration of
// the branching-time RECOVERABILITY differentiator CATCHING A REAL BUG — a lockup a bit-level,
// safety-only checker (SVA assertion, plain BMC) cannot express.
//
// This is the same "load → busy → done" core as compute_engine.sv, with ONE realistic defect: an
// `err` strobe from the environment during WORK drives the FSM into a FAULT state that has no
// transition back to IDLE — the designer forgot the recovery edge. Nothing SHORT OF A RESET gets
// the core out of FAULT, so once faulted it is BUSY FOREVER.
//
// The differentiator, in one line:
//     AG EF (busy == 0)   — "from EVERY reachable state, the core can STILL return to idle"
// On this design that property is *VIOLATED*: FAULT is a reachable state from which `busy==0` is
// unreachable (a trap). mununu returns a concrete counterexample path reset → … → FAULT.
//
// Why a safety/linear checker misses this:
//   - A bounded SVA `assert property (busy |-> ##[1:N] !busy)` needs a bound N; a genuine lockup
//     has no N, and picking one is guesswork. An unbounded `s_eventually !busy` is a LINEAR
//     liveness property (fairness-sensitive, often undecidable on an over-approximation).
//   - `AG EF !busy` is BRANCHING: "from every reachable state, there EXISTS a path back to idle."
//     A single failing linear trace cannot state it; only a branching-time engine can. mununu
//     decides it exactly over the bit-blasted state space (3-valued KMTS mu-calculus), soundly.
//
// Verified reset-pinned (`--pin rst_n=1`) on purpose: recovery must be via the design's
// OWN logic, not the reset escape. "Can it get unstuck WITHOUT a power-cycle?" — here, no.
//
// The properties are carried as `@mununu_guarantee` mu-calculus annotations (verified by
// `mununu sv verify-auto`). See README.md for the invocation and the verdict contrast.

// AG EF(idle): from every reachable state, can the core still return to idle?  VIOLATED (FAULT trap).
// @mununu_guarantee nu Y.((mu X.(busy==0 || <> X)) && [] Y)
// Non-vacuity witness: busy is genuinely reachable, so the VIOLATED above is a real trap, not vacuity.
// @mununu_guarantee mu Z.(busy==1 || <> Z)
module compute_engine_faulty #(
    parameter int WORK_CYCLES = 8
) (
    input  logic        clk,
    input  logic        rst_n,        // async, active-low
    input  logic        start,        // pulse to begin a computation
    input  logic        err,          // fault strobe from a sub-unit (drives the lockup)
    input  logic [31:0] data_in,
    output logic        busy,         // high while a computation is in flight
    output logic        done,         // one-cycle pulse when a result is ready
    output logic [31:0] result
);
    typedef enum logic [1:0] { IDLE, WORK, FINISH, FAULT } state_t;
    state_t     state;
    logic [7:0] cnt;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            state  <= IDLE;
            cnt    <= '0;
            result <= '0;
            done   <= 1'b0;
        end else begin
            done <= 1'b0;                       // default: a single-cycle pulse
            case (state)
                IDLE:
                    if (start) begin
                        state  <= WORK;
                        cnt    <= '0;
                        result <= data_in;
                    end
                WORK: begin
                    result <= result ^ (result << 1) ^ {24'b0, cnt};  // self-contained "processing"
                    if (err)
                        state <= FAULT;          // THE BUG: a fault during WORK locks the core...
                    else if (cnt == WORK_CYCLES[7:0] - 8'd1)
                        state <= FINISH;
                    else
                        cnt <= cnt + 8'd1;
                end
                FINISH: begin
                    done  <= 1'b1;               // result is ready this cycle
                    state <= IDLE;               // ... the good path returns to idle (the recovery)
                end
                FAULT: begin
                    state <= FAULT;              // ... and there is NO edge back to IDLE (the defect).
                end
                default: state <= IDLE;
            endcase
        end
    end

    assign busy = (state != IDLE);
endmodule
