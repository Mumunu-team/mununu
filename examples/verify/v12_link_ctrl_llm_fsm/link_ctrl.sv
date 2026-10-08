// link_ctrl — request/acknowledge link controller with retry, error and fatal handling.
// Generated from the spec in prompt.md. All outputs are registered.
module link_ctrl #(
    parameter int BEATS = 4
) (
    input  logic clk,
    input  logic rst_n,
    input  logic start,
    input  logic ack,
    input  logic nack,
    input  logic beat_done,
    input  logic clr_err,
    output logic req,
    output logic busy,
    output logic done,
    output logic err,
    output logic fatal
);

  // State encoding
  localparam logic [2:0] S_IDLE  = 3'd0;
  localparam logic [2:0] S_REQ   = 3'd1;
  localparam logic [2:0] S_XFER  = 3'd2;
  localparam logic [2:0] S_RETRY = 3'd3;
  localparam logic [2:0] S_ERR   = 3'd4;
  localparam logic [2:0] S_FATAL = 3'd5;

  localparam int MAX_RETRIES = 3;

  logic [2:0] state_q, state_d;
  logic [1:0] retry_cnt_q, retry_cnt_d;
  logic [2:0] beat_cnt_q, beat_cnt_d;

  logic req_q, busy_q, done_q, err_q, fatal_q;
  logic req_d, busy_d, done_d, err_d, fatal_d;

  // Protocol violation: ack and nack in the same cycle.
  logic violation;
  assign violation = ack & nack;

  // Next-state logic
  always_comb begin
    state_d     = state_q;
    retry_cnt_d = retry_cnt_q;
    beat_cnt_d  = beat_cnt_q;
    req_d       = 1'b0;
    busy_d      = 1'b1;
    done_d      = 1'b0;
    err_d       = 1'b0;
    fatal_d     = fatal_q;

    unique case (state_q)
      S_IDLE: begin
        busy_d = 1'b0;
        if (start) begin
          state_d     = S_REQ;
          retry_cnt_d = 2'd0;
          beat_cnt_d  = 3'd0;
          req_d       = 1'b1;
          busy_d      = 1'b1;
        end
      end

      S_REQ: begin
        req_d = 1'b1;
        if (violation) begin
          state_d = S_FATAL;
          req_d   = 1'b0;
          fatal_d = 1'b1;
        end else if (ack) begin
          state_d = S_XFER;
          req_d   = 1'b0;
        end else if (nack) begin
          req_d = 1'b0;
          if (retry_cnt_q == MAX_RETRIES) begin
            state_d = S_ERR;
            err_d   = 1'b1;
          end else begin
            state_d     = S_RETRY;
            retry_cnt_d = retry_cnt_q + 2'd1;
          end
        end
      end

      S_RETRY: begin
        // One idle cycle on the link, then re-issue the request.
        state_d = S_REQ;
        req_d   = 1'b1;
      end

      S_XFER: begin
        if (violation) begin
          state_d = S_FATAL;
          fatal_d = 1'b1;
        end else if (beat_done) begin
          if (beat_cnt_q == BEATS - 1) begin
            state_d    = S_IDLE;
            done_d     = 1'b1;
            busy_d     = 1'b0;
            beat_cnt_d = 3'd0;
          end else begin
            beat_cnt_d = beat_cnt_q + 3'd1;
          end
        end
      end

      S_ERR: begin
        err_d  = 1'b1;
        busy_d = 1'b0;
        if (clr_err) begin
          state_d = S_IDLE;
          err_d   = 1'b0;
        end
      end

      S_FATAL: begin
        // Protocol violation: stop. Only a reset leaves this state.
        fatal_d = 1'b1;
        busy_d  = 1'b0;
      end

      default: begin
        state_d = S_IDLE;
      end
    endcase
  end

  // Registers
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      state_q     <= S_IDLE;
      retry_cnt_q <= 2'd0;
      beat_cnt_q  <= 3'd0;
      req_q       <= 1'b0;
      busy_q      <= 1'b0;
      done_q      <= 1'b0;
      err_q       <= 1'b0;
      fatal_q     <= 1'b0;
    end else begin
      state_q     <= state_d;
      retry_cnt_q <= retry_cnt_d;
      beat_cnt_q  <= beat_cnt_d;
      req_q       <= req_d;
      busy_q      <= busy_d;
      done_q      <= done_d;
      err_q       <= err_d;
      fatal_q     <= fatal_d;
    end
  end

  assign req   = req_q;
  assign busy  = busy_q;
  assign done  = done_q;
  assign err   = err_q;
  assign fatal = fatal_q;

  // ------------------------------------------------------------------
  // Assertion suite
  // ------------------------------------------------------------------
  // req is only driven while the controller is busy.
  a_req_only_when_busy: assert property (@(posedge clk) disable iff (!rst_n)
    req_q |-> busy_q);

  // done is a single-cycle pulse.
  a_done_single_pulse: assert property (@(posedge clk) disable iff (!rst_n)
    done_q |=> !done_q);

  // err is only raised once the retry limit has been reached.
  a_err_implies_retry_limit: assert property (@(posedge clk) disable iff (!rst_n)
    err_q |-> retry_cnt_q == MAX_RETRIES);

  // The retry counter never exceeds the limit.
  a_retry_limit: assert property (@(posedge clk) disable iff (!rst_n)
    !(retry_cnt_q == 2'd3 && state_q == S_RETRY));

  // fatal is sticky: once raised it stays raised.
  a_fatal_sticky: assert property (@(posedge clk) disable iff (!rst_n)
    fatal_q |=> fatal_q);

  // Beats are only counted in the transfer state.
  a_no_beats_outside_xfer: assert property (@(posedge clk) disable iff (!rst_n)
    !(state_q == S_XFER) |=> beat_cnt_q == $past(beat_cnt_q) || beat_cnt_q == 3'd0);

  // In idle, nothing is pending.
  a_idle_quiet: assert property (@(posedge clk) disable iff (!rst_n)
    state_q == S_IDLE |-> !req_q && !busy_q);

  // The state register never holds an illegal encoding.
  a_state_legal: assert property (@(posedge clk) disable iff (!rst_n)
    !(state_q == 3'd6) && !(state_q == 3'd7));

endmodule
