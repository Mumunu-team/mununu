// Local-variable properties over an enum domain — the `ASSERT_FPV_LINEAR_FSM` shape of OpenTitan's
// prim_assert.sv:169-185 (rom_ctrl_fsm, keymgr_ctrl), expanded by `assertion_rewrite` over the state's
// enum members; the `until` family in a consequent; the refusals. Captured 2026-10-23 in the
// `mununu-sva` image (slang 11.0.448+e222e7dc0):
//   slang --quiet --single-unit --ast-json sva_local_var_forms.ast.json --top forms sva_local_var_forms.sv
// Where things sit in the JSON (read after the capture, pinned by the translator tests):
//   - the enum: a `TypeAlias` whose `target` names an `EnumType` node; its `members[]` are `EnumValue`
//     nodes whose `value` is the folded constant text (the concatenation initialisers fold to `10'b…`).
//   - the local var: `localVars[] = {name, kind: LocalAssertionVar, type: "<alias text>"}` on the
//     property declaration; its one assignment is the `Assignment` match item of `SequenceWithMatch`.
//   - the ops: `Binary.op` ∈ {Until, SUntil, UntilWith, SUntilWith}; the folded `(0) !== '0` is the
//     `DisableIff.condition` with a `constant`.
package forms_pkg;
  localparam logic [3:0] MuBi4True  = 4'h6;
  localparam logic [3:0] MuBi4False = 4'h9;
  // rom_ctrl_pkg's sparse encoding: a mubi nibble inside each member (concatenation initialisers)
  typedef enum logic [9:0] {
    ReadingLow  = {6'b001100, MuBi4False},
    ReadingHigh = {6'b001011, MuBi4False},
    RomAhead    = {6'b111001, MuBi4False},
    KmacAhead   = {6'b100111, MuBi4False},
    Checking    = {6'b010101, MuBi4False},
    Done        = {6'b100000, MuBi4True},
    Invalid     = {6'b010010, MuBi4False}
  } fsm_state_e;
endpackage

module forms import forms_pkg::*; (
  input  logic clk_i, rst_ni,
  input  logic step_i, stop_i, ready_i,
  input  logic [2:0] idx_i,
  output logic [9:0] state_o
);
  fsm_state_e state_q, state_d;
  always_comb begin
    state_d = state_q;
    if (step_i) begin
      unique case (state_q)
        ReadingLow:  state_d = ReadingHigh;
        ReadingHigh: state_d = RomAhead;
        RomAhead:    state_d = Checking;
        KmacAhead:   state_d = Checking;
        Checking:    state_d = Done;
        default:     state_d = Invalid;
      endcase
    end
  end
  always_ff @(posedge clk_i or negedge rst_ni) begin
    if (!rst_ni) state_q <= ReadingLow; else state_q <= state_d;
  end
  assign state_o = state_q;

  // (i) ASSERT_FPV_LINEAR_FSM(SecCmCFILinear_A, state_q, fsm_state_e) — the macro's expansion verbatim
  // (prim_assert.sv:169-185 with `ASSERT(__name, __name``_p, __clk, 0)` → `disable iff ((0) !== '0)`)
  bit SecCmCFILinear_A_cond;
  always_ff @(posedge clk_i or posedge !rst_ni) begin
    if (!rst_ni) begin
      SecCmCFILinear_A_cond <= 0;
    end else begin
      SecCmCFILinear_A_cond <= 1;
    end
  end
  property SecCmCFILinear_A_p;
    fsm_state_e initial_state;
    (!$stable(state_q) & SecCmCFILinear_A_cond, initial_state = $past(state_q)) |->
        (state_q != initial_state) until !(SecCmCFILinear_A_cond);
  endproperty
  SecCmCFILinear_A: assert property (@(posedge clk_i) disable iff ((0) !== '0) (SecCmCFILinear_A_p));

  // (ii) the same shape over a packed 3-bit local variable (domain 2^3)
  property LinearPacked_P;
    logic [2:0] init_s;
    (!$stable(idx_i), init_s = $past(idx_i)) |-> (idx_i != init_s) until !ready_i;
  endproperty
  LinearPacked_A: assert property (@(posedge clk_i) disable iff (!rst_ni) LinearPacked_P);

  // (iii) the `until` family on boolean operands, no local variable
  UntilWeak_A:    assert property (@(posedge clk_i) disable iff (!rst_ni) step_i |-> ready_i until stop_i);
  UntilStrong_A:  assert property (@(posedge clk_i) disable iff (!rst_ni) step_i |-> ready_i s_until stop_i);
  UntilWith_A:    assert property (@(posedge clk_i) disable iff (!rst_ni) step_i |-> ready_i until_with stop_i);
  UntilWithS_A:   assert property (@(posedge clk_i) disable iff (!rst_ni) step_i |-> ready_i s_until_with stop_i);

  // (iv) a property-position `and` of two implications
  property BothP;
    (step_i |-> ##1 ready_i) and (stop_i |-> ##1 !ready_i);
  endproperty
  Both_A: assert property (@(posedge clk_i) disable iff (!rst_ni) BothP);

  // (v) refusals: a local assigned twice; one read in the antecedent; one assigned from another
  property TwiceP;
    logic [2:0] v;
    (step_i, v = idx_i) ##1 (ready_i, v = idx_i) |-> idx_i == v;
  endproperty
  Twice_A: assert property (@(posedge clk_i) disable iff (!rst_ni) TwiceP);
  property ReadInAnteP;
    logic [2:0] v;
    (step_i, v = idx_i) ##1 (idx_i == v) |-> ready_i;
  endproperty
  ReadInAnte_A: assert property (@(posedge clk_i) disable iff (!rst_ni) ReadInAnteP);
  property ChainP;
    logic [2:0] a, b;
    (step_i, a = idx_i, b = a) |-> idx_i != b;
  endproperty
  Chain_A: assert property (@(posedge clk_i) disable iff (!rst_ni) ChainP);
endmodule
