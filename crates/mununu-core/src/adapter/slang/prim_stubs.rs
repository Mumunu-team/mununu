//! H.C — behavioral stubs for OpenTitan flop primitives, auto-injected by
//! [`verify_auto`](crate::adapter::slang::verify_auto) when the lift reports the
//! corresponding module was cut (instantiated with no body).
//!
//! OpenTitan wraps registers in `prim_flop`-family primitives whose bodies are
//! not in a single-module source set. Yosys then leaves them as dangling
//! undefined-module cells and the register vanishes from the lift (the csrng
//! `prim_sparse_fsm_flop` case). These stubs model the **verification-relevant**
//! behavior — an async-reset register whose output is the registered input —
//! and intentionally drop the security hardening (sparse-encoding alerts), which
//! is orthogonal to functional/temporal properties over the registered state.
//!
//! **Soundness.** Each stub is an *exact* behavioral model of the flop's
//! datapath (`state_o`/`q_o` = the async-reset register of `state_i`/`d_i`); it
//! adds a real register, never an abstraction. The dropped hardening only
//! removes alert side-channels, not the registered value the SVA reasons about.
//! Auto-injection is reported in the verify-auto diagnostics so it is never
//! silent.

/// Behavioral `prim_sparse_fsm_flop` — the OpenTitan FSM-state flop. Interface
/// matches the `PRIM_FLOP_SPARSE_FSM` macro's non-SIMULATION instantiation
/// (`crates/.../prim_flop_macros.sv`): params `StateEnumT/Width/ResetValue/
/// EnableAlertTriggerSVA`, ports `clk_i/rst_ni/state_i/state_o`.
const PRIM_SPARSE_FSM_FLOP: &str = r#"// Auto-injected behavioral model (mununu verify-auto H.C). Models the
// registered datapath of OpenTitan's prim_sparse_fsm_flop; drops the
// sparse-encoding security hardening (orthogonal to FSM-state properties).
module prim_sparse_fsm_flop #(
  parameter type               StateEnumT            = logic,
  parameter int                Width                 = 1,
  parameter logic [Width-1:0]  ResetValue            = '0,
  parameter bit                EnableAlertTriggerSVA = 1
) (
  input  logic             clk_i,
  input  logic             rst_ni,
  input  logic [Width-1:0] state_i,
  output logic [Width-1:0] state_o
);
  logic [Width-1:0] state_q;
  always_ff @(posedge clk_i or negedge rst_ni) begin
    if (!rst_ni) state_q <= ResetValue;
    else         state_q <= state_i;
  end
  assign state_o = state_q;
endmodule
"#;

/// Behavioral `prim_flop` — the plain OpenTitan register primitive. Interface
/// matches the `PRIM_FLOP` macro: params `Width/ResetValue`, ports
/// `clk_i/rst_ni/d_i/q_o`.
const PRIM_FLOP: &str = r#"// Auto-injected behavioral model (mununu verify-auto H.C). Models the
// registered datapath of OpenTitan's prim_flop.
module prim_flop #(
  parameter int               Width      = 1,
  parameter logic [Width-1:0] ResetValue = '0
) (
  input  logic             clk_i,
  input  logic             rst_ni,
  input  logic [Width-1:0] d_i,
  output logic [Width-1:0] q_o
);
  logic [Width-1:0] q;
  always_ff @(posedge clk_i or negedge rst_ni) begin
    if (!rst_ni) q <= ResetValue;
    else         q <= d_i;
  end
  assign q_o = q;
endmodule
"#;

/// The behavioral stub SV for a known flop-primitive module, or `None` if there
/// is no stub for `module_name`, the module name as instantiated (see
/// [`crate::adapter::yosys`]'s undefined-cell detection).
pub fn behavioral_stub(module_name: &str) -> Option<&'static str> {
    match module_name {
        "prim_sparse_fsm_flop" => Some(PRIM_SPARSE_FSM_FLOP),
        "prim_flop" => Some(PRIM_FLOP),
        _ => None,
    }
}

/// For a set of cut (undefined-body) module names, return the
/// `(file_name, stub_sv)` sources to inject for those that have a behavioral
/// stub. The file name is `<module>.sv`. Deterministic order (sorted by name).
pub fn stubs_for_cut_modules(cut: &[String]) -> Vec<(String, String)> {
    let mut names: Vec<&String> = cut.iter().collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter_map(|m| behavioral_stub(m).map(|sv| (format!("{m}.sv"), sv.to_string())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_flop_primitives_have_stubs() {
        assert!(behavioral_stub("prim_sparse_fsm_flop").is_some());
        assert!(behavioral_stub("prim_flop").is_some());
        assert!(behavioral_stub("some_user_module").is_none());
    }

    #[test]
    fn stubs_for_cut_modules_filters_and_names() {
        let cut = vec![
            "prim_sparse_fsm_flop".to_string(),
            "user_blackbox".to_string(),
        ];
        let stubs = stubs_for_cut_modules(&cut);
        assert_eq!(stubs.len(), 1, "only the known prim is stubbed");
        assert_eq!(stubs[0].0, "prim_sparse_fsm_flop.sv");
        assert!(stubs[0].1.contains("module prim_sparse_fsm_flop"));
    }
}
