// Properties for `pulse_cdc`, bound in rather than written in the design file.
//
// `yosys`'s `read_verilog -sv` cannot parse `assert property` at all, and
// `mununu sv verify-auto` has no `--define`, so an `ifdef` guard would hide the
// assertions from the VERIFIER too. A separate file bound with `bind ... (.*)`
// is visible to one and invisible to the other.
/* verilator lint_off UNUSEDSIGNAL */
module pulse_cdc_sva (
    input logic clk_src,
    input logic rst_src_n,
    input logic pulse_in,
    input logic clk_dst,
    input logic rst_dst_n,
    input logic pulse_out,
    // Internals the properties name. `bind ... (.*)` connects by NAME, so a
    // signal a property refers to must be declared here too.
    input logic tog_q,
    input logic meta_q,
    input logic sync_q,
    input logic sync_d_q
);
/* verilator lint_on UNUSEDSIGNAL */

    // ---------------------------------------------------------------------
    // TIER 1 — safety
    // ---------------------------------------------------------------------

    // **`a_out_is_one_cycle` WAS HERE AND IS REMOVED — THE MODEL CHECKER WAS
    // RIGHT AND I WAS WRONG.**
    //
    // It asserted `(pulse_out == 1) |=> (pulse_out == 0)`: the output is never
    // high two cycles running, which is the fault this block exists to fix and
    // reads as the most obviously true thing in the file. mununu returned
    // VIOLATED on the CORRECT design, and the counterexample is the point: against
    // a FREE environment the two clocks are unconstrained, so the source may
    // toggle twice inside one destination cycle and the far side sees two edges
    // back to back. No behaviour of this block prevents that.
    //
    // That is not a defect, it is the RATE OBLIGATION — `pulse_in` rarer than the
    // destination's synchroniser — and `rosf.toml`'s `[environment]` block is
    // where rule 3 says such an obligation lives. A property cannot express
    // "rarely", and a model checker asked this question answers it about a
    // schedule nobody will run.
    //
    // CLAUDE.md names this exact trap: *"the assertions that read as most
    // obviously true are the ones most likely to be misplaced."* `wb_mem_client`
    // had three of them. This was a fourth.
    //
    // **AND BEFORE DELETING IT, AN ASSUMPTION WAS TRIED AND MEASURED —
    // 2026-10-03. The shape is why it did not help, and that is worth knowing.**
    //
    // The rate obligation has a SAFETY shape here: "only one event is in flight",
    // i.e. the source does not toggle again while the previous toggle is still
    // propagating. Written as a state predicate:
    //
    //     nu A. (((tog_q == meta_q) || (meta_q == sync_q)) && [] A)
    //
    // **mununu RECORDED it and did not APPLY it, and said so in its transcript:**
    // `@mununu_assume` of the form `<signal> = <value>` is applied by config
    // concretization, and a temporal `GF <REG op VALUE>` fairness assume IS
    // auto-applied for response-shape guarantees `AG(a -> AF b)` (mununu#477
    // Option B, Emerson-Lei fair-cycle l2s). **Anything else — a STATE PREDICATE
    // like this one, a non-response guarantee shape, or multi-guarantee-coupled
    // fairness — is recorded only.**
    //
    // So the property comes out, but for a precise reason rather than a shrug:
    // the environment constraint that would rescue it is not of a shape this path
    // can apply. The engine named two routes that can —
    // `mununu btor2 verify-liveness-under-fairness` on the emitted BTOR2, or a
    // CTXDSL model where the GR(1) game engine discharges it — and neither is
    // worth a block this size.
    //
    // The obligation therefore lives in `rosf.toml`'s `[environment]`, which is
    // where rule 3 puts an obligation a block cannot own.

    // What remains below is what the BLOCK owns regardless of its environment.

    // The output is exactly the edge, stated as the design's own definition so a
    // later change to the expression has to disagree with a property rather than
    // merely with a comment.
    //
    // NOT a tautology, although it reads like CLAUDE.md's example of one: the
    // twin `faulty/pulse_cdc_level.sv` (a LEVEL, not an edge) VIOLATES it, so the
    // property has content the design text alone does not. Said here because
    // since 2026-10-08 the portfolio returns ⊥ for it — the exact engine HOLDS,
    // the explicit engine VIOLATED, a contradiction the engine forces to ⊥
    // (mununu#637) — and verify.sh pins that ⊥ by name until upstream decides
    // which engine is wrong. Deleting the property would have been the easy
    // green and would have taken the twin's only flipped assertion with it.
    a_out_is_the_edge: assert property (
        @(posedge clk_dst) disable iff (!rst_dst_n)
        (pulse_out == 1'b1) |-> (sync_q != sync_d_q)
    );

    // The toggle does not move without a pulse. This is the source-side half of
    // the contract and it is what makes "one far-side edge per source event"
    // mean anything.
    a_toggle_moves_only_on_a_pulse: assert property (
        @(posedge clk_src) disable iff (!rst_src_n)
        (pulse_in == 1'b0) |=> (tog_q == $past(tog_q))
    );

    // ---------------------------------------------------------------------
    // TIER 3 — recoverability, with its two-sided non-vacuity witness (rule 4)
    // ---------------------------------------------------------------------
    //
    // "However the crossing is driven, a state in which the destination is quiet
    // is always reachable again." A crossing that can wedge with the output stuck
    // is the failure this says cannot happen.
    //
    // @mununu_guarantee nu X. ((mu Y. ((pulse_out == 0) || (<> Y))) && ([] X))

    // THE WITNESS, and without it the formula above holds trivially on a design
    // whose output is stuck at zero — which is precisely the dead-instrument
    // shape rule 23 names. `EF (pulse_out == 1)` is what makes the first
    // property say something.
    //
    // @mununu_guarantee mu Z. ((pulse_out == 1) || (<> Z))

endmodule

bind pulse_cdc pulse_cdc_sva u_pulse_cdc_sva (.*);
