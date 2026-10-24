// compute_engine.sv — a self-contained "load → busy → done" compute core.
//
// Authored for mununu as a PUBLIC, permissively-licensed (this repo's license) reproducer of
// the branching-time RECOVERABILITY differentiator — the class of property no bit-level,
// safety-only tool (SVA assertion checker, plain BMC) can express.
//
// The differentiator, in one line:
//     AG EF (busy == 0)   — "from EVERY reachable state, the core can return to idle"
// paired with its non-vacuity witness  EF (busy == 1)  ("the core genuinely goes busy", so the
// recovery is not trivially true). A safety checker can only say "busy is low on THIS trace";
// only a branching-time engine says "idle is ALWAYS re-reachable, from anywhere". mununu decides
// it exactly over the bit-blasted state space (3-valued KMTS mu-calculus), soundly.
//
// Why this design is a *meaningful* (non-vacuous) example — the property gate mununu's own work
// applies (see the benchmark-property discipline): the recovery target `busy==0` is genuinely
// LEFT (busy goes high on `start`) and always re-reached (the FSM returns to IDLE), so the HOLDS
// is a real recoverability claim, not a stuck-signal vacuity. Its activity is self-contained: the
// WORK phase advances on its own clock with no external handshake, so the whole busy↔idle cycle is
// reachable in module isolation.
//
// The properties are carried as `@mununu_guarantee` mu-calculus annotations (verified by
// `mununu sv verify-auto`). Run out-of-reset (`--pin rst_n=1`) — see README.md.

// AG EF(idle): from every reachable state, the core can return to idle (the recovery).
// @mununu_guarantee nu Y.((mu X.(busy==0 || <> X)) && [] Y)
// Non-vacuity witness: busy is genuinely reachable, so the recovery above is not trivially true.
// @mununu_guarantee mu Z.(busy==1 || <> Z)
// AG EF(done): from every reachable state, the core can always complete a computation.
// @mununu_guarantee nu Y.((mu X.(done==1 || <> X)) && [] Y)
module compute_engine #(
    parameter int WORK_CYCLES = 8
) (
    input  logic        clk,
    input  logic        rst_n,        // async, active-low
    input  logic        start,        // pulse to begin a computation
    input  logic [31:0] data_in,
    output logic        busy,         // high while a computation is in flight
    output logic        done,         // one-cycle pulse when a result is ready
    output logic [31:0] result
);
    typedef enum logic [1:0] { IDLE, WORK, FINISH } state_t;
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
                    if (cnt == WORK_CYCLES[7:0] - 8'd1)
                        state <= FINISH;
                    else
                        cnt <= cnt + 8'd1;
                end
                FINISH: begin
                    done  <= 1'b1;               // result is ready this cycle
                    state <= IDLE;               // ... and the core returns to idle (the recovery)
                end
                default: state <= IDLE;
            endcase
        end
    end

    assign busy = (state != IDLE);
endmodule
