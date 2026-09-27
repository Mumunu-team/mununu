//! Reset-gated initial state — inject BTOR2 `init` lines at the post-reset state.
//!
//! An async-reset flop (`always_ff @(posedge clk or negedge rst_ni)
//! if (!rst_ni) q <= ResetValue; else q <= d;`) lifts — via Yosys `async2sync` —
//! to a next-state mux `next(q) = ite(rst, ResetValue, d)` with **no `init`
//! line**: the *reset*, not a power-on value, establishes the start state.
//! verify-auto then PINS the reset input to its inactive level (the "verified
//! out of reset" discipline), so the mux never selects `ResetValue`, and both
//! verify-auto engines default the init-less register to 0
//! ([`crate::adapter::btor2::symbolic_bitblast`]'s `initial_state_bdd` and the
//! predicate-cube path's `state_cell_init_values`, per the `setundef -zero`
//! convention).
//!
//! For a design whose valid initial state is established BY reset — e.g. an
//! OpenTitan sparse-FSM whose `ResetValue` is a non-zero sparse encoding
//! (`MainSmIdle = 6'b110111`) — starting at 0 lands on an **illegal encoding**,
//! which the FSM's `default` arm traps into its error state. Every
//! reset-dependent verdict (recoverability `AG EF idle`, liveness-from-reset) is
//! then computed from a state the real design never occupies.
//!
//! This transformation restores the intended semantics. BEFORE the reset is
//! pinned inactive, it derives the post-reset state by simulating one cycle with
//! the reset ASSERTED (the same 1-cycle hold as
//! [`crate::adapter::btor2::bit_blast`]'s F1.1 `apply_auto_reset`, which does the
//! equivalent for the enum/CTXDSL path) and appends an `init` line per state
//! cell at that value. Working at the BTOR2 **text** level means both
//! verify-auto engines — which each re-parse the text and read `init` lines —
//! pick up the reset state with no per-engine change.
//!
//! Scope guard: only fires when reset-gating is on (`resets` non-empty — empty
//! under `--no-gate-reset`, where the design chooses its own power-up, including
//! the undefined-encoding scenarios CWE-1245 detection relies on). Eligibility is
//! then decided **per state cell** (mununu#578): a cell that already carries an
//! `init` line is authoritative and untouched, an array-sorted cell is skipped (a
//! `constd` init is ill-typed for one), and a cell with no `next` is skipped
//! because it is free by construction rather than by reset. The guard used to be
//! per-DESIGN — one `init` anywhere disabled the pass — which meant a single
//! yosys-emitted **memory** init left every async-reset flop in the design free at
//! cycle 0. That is mununu#577: a free cycle-0 state over-approximates
//! reachability, which licenses a definite HOLDS but never a definite VIOLATED,
//! and it produced confident violations of properties the hardware satisfies.

use crate::adapter::AdapterError;
use crate::adapter::btor2::ast::{Nid, Node, Sort};
use crate::adapter::btor2::bit_blast::simulate_one_step;
use crate::adapter::btor2::parser;
use std::collections::HashMap;

/// Inject `init` lines at the post-reset state for a reset-gated async-reset
/// design that has none. `resets` is the set of `(name, inactive_value)` reset
/// pins verify-auto detected — the same set it pins inactive.
///
/// No-op (returns `content` unchanged) when `resets` is empty or no state cell is eligible.
///
/// Eligibility is per cell (mununu#578). A cell is skipped when it already carries an `init`
/// (authoritative — must not be advanced past, matching `apply_auto_reset`'s guard), when its sort
/// is an array (a `constd` init is ill-typed, and a memory's power-up is not a scalar reset value),
/// or when it has no `next` (a `--cutpoint` or blackboxed output: free at every cycle by
/// deliberate construction, and pinning cycle 0 would narrow that abstraction — which removes
/// start states, and so would be unsound for HOLDS rather than for VIOLATED).
///
/// The post-reset state is `simulate_one_step` from the all-zero power-on cube
/// with each reset input pinned to its ASSERTED level (the complement of its
/// inactive level). A reset-register's next-state mux then selects its
/// `ResetValue`; a register with no reset advances one cycle from 0 (its
/// post-reset value is whatever a held-reset cycle yields — the faithful "one
/// reset cycle then release" state).
pub fn inject_reset_init(content: &str, resets: &[(String, u64)]) -> Result<String, AdapterError> {
    if resets.is_empty() {
        return Ok(content.to_string());
    }

    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/reset_init: {}", e.message);
        e
    })?;

    // mununu#578 — the eligibility guard is PER STATE CELL, not per design.
    //
    // It used to be `if any line is an Init { return unchanged }`, which reads as "a design with
    // an authoritative BTOR2 init must not be advanced past". The intent is right; the scope was
    // not. Yosys emits an `init` for **every memory** it lifts, so one array init — a line that
    // says nothing whatever about the scalar flops — switched the whole pass off. Measured on the
    // design that found this (mununu#577): ONE `init`, for an array, against TEN init-less
    // async-reset registers, every one of them left free at cycle 0. Those free registers then
    // produced a confident `AG(drop_q <= 1) VIOLATED` on RTL that bounds `drop_q` at 1.
    //
    // Per-cell is also what the sibling `inject_zero_init` below already does, and the two now
    // agree on which cells they may touch.
    let has_init: std::collections::HashSet<Nid> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Init { state, .. } => Some(*state),
            _ => None,
        })
        .collect();

    // Array-sorted states are skipped: a `constd` init is ill-typed for one, and a memory's
    // power-up is not a scalar reset value. (Same exclusion as `inject_zero_init`.) This is not
    // merely tidiness — it is what the coarse guard was accidentally providing, since an
    // array-bearing design never reached this loop at all.
    let bitvec_sorts: std::collections::HashSet<Nid> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Sort {
                sort: Sort::BitVec { .. },
            } => Some(l.nid),
            _ => None,
        })
        .collect();

    // A state with NO `next` is free at EVERY cycle by construction — a `--cutpoint`, or a
    // blackboxed submodule output. Nothing about a reset applies to it, and pinning its cycle-0
    // value would silently narrow an abstraction the user asked for. Narrowing removes start
    // states, so unlike the defect above it would be unsound for HOLDS — the opposite direction,
    // and the more dangerous one.
    let has_next: std::collections::HashSet<Nid> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Next { state, .. } => Some(*state),
            _ => None,
        })
        .collect();

    // State cells (nid, sort, key). Yosys leaves the async2sync FSM register
    // unnamed; `simulate_one_step` keys those by `st_n<nid>`, so mirror that.
    let symbols = parser::collect_symbols(&file);
    let states: Vec<(Nid, Nid, String)> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::State { sort, .. }
                if !has_init.contains(&l.nid)
                    && bitvec_sorts.contains(sort)
                    && has_next.contains(&l.nid) =>
            {
                let key = symbols
                    .get(&l.nid)
                    .cloned()
                    .unwrap_or_else(|| format!("st_n{}", l.nid));
                Some((l.nid, *sort, key))
            }
            _ => None,
        })
        .collect();
    if states.is_empty() {
        return Ok(content.to_string());
    }

    // Reset ASSERTED = complement of the inactive level (resets are 1-bit).
    // Registers default 0 (setundef -zero power-on); non-reset inputs default 0.
    let reset_inputs: HashMap<String, u128> = resets
        .iter()
        .map(|(name, inactive)| {
            let asserted: u128 = if *inactive == 0 { 1 } else { 0 };
            (name.clone(), asserted)
        })
        .collect();
    // mununu#578 — simulation is the PREFERRED reader, not the only one.
    //
    // `simulate_one_step` runs the Phase-1 bit-blaster, which does not implement the array
    // operators: a design that READS a memory fails with *"operator Read unsupported"*. That never
    // surfaced before because the per-design guard above turned the whole pass off for any design
    // with a memory — the guard was quietly doing a second job nobody had written down, and
    // narrowing it to the job it documents exposed the other one.
    //
    // Propagating that error is not an option: it would fail the run outright for a large class of
    // ordinary RTL. Nor is giving up, which is what left mununu#577's registers free.
    //
    // So when the model cannot be simulated, read each reset value directly off the **arm of the
    // reset mux that the pin will make dead**. `async2sync` emits `next(q) = ite(rst, d, RESET)`,
    // so the reset value is sitting there as a literal; a register whose dead arm is not a constant
    // is skipped rather than guessed. This reads strictly less than simulation (it cannot evaluate
    // a computed reset, and it does not advance a reset-less register by a cycle), which is why it
    // is the fallback and not the default.
    let appended = match simulate_one_step(&file, &HashMap::new(), &reset_inputs) {
        Ok(post_reset) => {
            let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
            let mut appended: Vec<String> = Vec::new();
            for (state_nid, sort_nid, key) in &states {
                let value = post_reset.get(key).copied().unwrap_or(0);
                let const_nid = next_nid;
                next_nid += 1;
                let init_nid = next_nid;
                next_nid += 1;
                appended.push(format!("{const_nid} constd {sort_nid} {value}"));
                appended.push(format!(
                    "{init_nid} init {sort_nid} {state_nid} {const_nid}"
                ));
            }
            appended
        }
        Err(e) => {
            tracing::debug!(
                error = %e.message,
                "mununu#578: model not simulable (array ops are outside the Phase-1 bit-blaster); \
                 reading reset values off the reset mux instead"
            );
            reset_init_from_mux_arms(&file, &states, resets)
        }
    };
    if appended.is_empty() {
        return Ok(content.to_string());
    }

    Ok(format!("{}\n{}\n", content.trim_end(), appended.join("\n")))
}

/// Read each register's reset value off the arm of its reset mux that the reset pin will kill.
///
/// Used when [`simulate_one_step`] cannot run the model — in practice, whenever the design reads a
/// memory, since the array operators are outside the Phase-1 bit-blaster. Returns the `constd` +
/// `init` line pairs to append, which is empty when nothing is recoverable.
///
/// **Polarity is derived, not special-cased.** The pin ties the reset to its *inactive* level, so
/// the arm that level does NOT select is what the design does while the reset is asserted — i.e.
/// the reset value. An active-low `rst_n` pinned to 1 gives `ite(rst_n, d, RESET)` → the `else`
/// arm; an active-high `rst` pinned to 0 gives `ite(rst, RESET, d)` → the `then` arm. A negated
/// operand flips the selection.
///
/// A register whose dead arm is not a literal constant is **skipped**: `next = ite(rst, d, q)` is a
/// flop that holds through reset and genuinely has no reset value, and anything computed is not a
/// value this pass can name. Skipping leaves it free, which the mununu#577 guard then reports
/// honestly — far better than inventing a zero.
fn reset_init_from_mux_arms(
    file: &crate::adapter::btor2::ast::Btor2File,
    states: &[(Nid, Nid, String)],
    resets: &[(String, u64)],
) -> Vec<String> {
    use crate::adapter::btor2::ast::{ConstValue, Op};

    // Reset INPUT nids — this runs before the pin, so a reset is still a free input.
    let symbols = parser::collect_symbols(file);
    let want: HashMap<&str, u64> = resets.iter().map(|(n, v)| (n.as_str(), *v)).collect();
    let reset_nids: HashMap<Nid, u64> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Input { .. } => {
                let sym = symbols.get(&l.nid)?;
                Some((l.nid, *want.get(sym.as_str())?))
            }
            _ => None,
        })
        .collect();
    if reset_nids.is_empty() {
        return Vec::new();
    }

    let next_of: HashMap<Nid, Nid> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Next { state, value, .. } => Some((*state, value.nid())),
            _ => None,
        })
        .collect();

    let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut appended: Vec<String> = Vec::new();
    for (state_nid, sort_nid, _) in states {
        let Some(next_fn) = next_of.get(state_nid).and_then(|n| file.lookup(*n)) else {
            continue;
        };
        let Node::Op {
            op: Op::Ite, args, ..
        } = &next_fn.node
        else {
            continue;
        };
        let [cond, then_arm, else_arm] = args.as_slice() else {
            continue;
        };
        let Some(&inactive) = reset_nids.get(&cond.nid()) else {
            continue;
        };
        let selects_then = if cond.is_negated() {
            inactive == 0
        } else {
            inactive != 0
        };
        let dead = if selects_then { else_arm } else { then_arm };
        // Only an UNNEGATED literal. A negated operand is a bit-inversion of the node, which is not
        // a shape yosys emits for a reset arm; declining keeps this to the shape it was measured on.
        if dead.is_negated() {
            continue;
        }
        let Some(line) = file.lookup(dead.nid()) else {
            continue;
        };
        let Node::Const { value, .. } = &line.node else {
            continue;
        };
        let decimal: u128 = match value {
            ConstValue::Zero => 0,
            ConstValue::One => 1,
            ConstValue::Bin(b) => match u128::from_str_radix(b, 2) {
                Ok(v) => v,
                Err(_) => continue,
            },
            ConstValue::Hex(h) => match u128::from_str_radix(h, 16) {
                Ok(v) => v,
                Err(_) => continue,
            },
            ConstValue::Dec(d) if *d >= 0 => *d as u128,
            // `ones` needs the sort width to render as a decimal, and a negative `constd` is not a
            // reset literal yosys emits. Skip rather than mis-render.
            _ => continue,
        };
        let const_nid = next_nid;
        next_nid += 1;
        let init_nid = next_nid;
        next_nid += 1;
        appended.push(format!("{const_nid} constd {sort_nid} {decimal}"));
        appended.push(format!(
            "{init_nid} init {sort_nid} {state_nid} {const_nid}"
        ));
    }
    appended
}

/// Complete the BTOR2 init to the `setundef -zero` power-on: append an `init … 0`
/// line for every BITVEC state cell that carries no `init` line.
///
/// **Why.** The cube and exact engines already default an init-less state cell to
/// 0 (`state_cell_init_values` / `initial_state_bdd`, per the `setundef -zero`
/// power-up). The reachability portfolio (native BMC / spacer / Boolector),
/// however, leaves an init-less cell FREE — BTOR2's nondeterministic-init
/// semantics. On a reset-less design with a *partial* `initial` (a Xilinx-style
/// wrapper: `initial state = IDLE` but an init-less status flop), that mismatch
/// hands the portfolio a power-up counterexample the exact engine never sees — a
/// verdict DISAGREEMENT (portfolio `VIOLATED` at 0 cells vs exact `HOLDS`).
/// Making the 0 power-up EXPLICIT in the BTOR2 puts every engine on the same
/// initial state.
///
/// **Scope / soundness.** Only for the reset-gated verify-auto path (its lift is
/// the `setundef -zero` model the cube/exact already assume); raw `btor2 verify`
/// keeps BTOR2's free-init semantics. Existing `init` lines — authoritative
/// `initial` values, or [`inject_reset_init`]'s post-reset state — are LEFT
/// UNTOUCHED; only init-less cells are completed. Array-sorted states are skipped
/// (a `constd` init is ill-typed for them).
pub fn inject_zero_init(content: &str) -> Result<String, AdapterError> {
    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/reset_init(zero-init): {}", e.message);
        e
    })?;

    // Sort nids that are bitvec — array-sorted states are skipped.
    let bitvec_sorts: std::collections::HashSet<Nid> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Sort {
                sort: Sort::BitVec { .. },
            } => Some(l.nid),
            _ => None,
        })
        .collect();

    // States that already carry an `init` line stay authoritative.
    let has_init: std::collections::HashSet<Nid> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Init { state, .. } => Some(*state),
            _ => None,
        })
        .collect();

    let initless: Vec<(Nid, Nid)> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::State { sort, .. }
                if !has_init.contains(&l.nid) && bitvec_sorts.contains(sort) =>
            {
                Some((l.nid, *sort))
            }
            _ => None,
        })
        .collect();
    if initless.is_empty() {
        return Ok(content.to_string());
    }

    let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut appended: Vec<String> = Vec::new();
    for (state_nid, sort_nid) in &initless {
        let const_nid = next_nid;
        next_nid += 1;
        let init_nid = next_nid;
        next_nid += 1;
        appended.push(format!("{const_nid} constd {sort_nid} 0"));
        appended.push(format!(
            "{init_nid} init {sort_nid} {state_nid} {const_nid}"
        ));
    }
    Ok(format!("{}\n{}\n", content.trim_end(), appended.join("\n")))
}

#[cfg(test)]
mod tests {
    /// mununu#578 — a MEMORY's `init` must not switch reset-init off for the scalar flops.
    ///
    /// This is the shape that produced mununu#577, reduced from the real dump: an array state
    /// carrying the `init` yosys emits for every memory, beside an async-reset register carrying
    /// none. The old guard was per-design (`if any Init exists { return unchanged }`), so that one
    /// array line left `q` free at cycle 0 — and a free cycle-0 state over-approximates
    /// reachability, which makes a `VIOLATED` unsound.
    ///
    /// The assertion is that `q` gets its RESET value (5), not merely that some init appeared: an
    /// implementation that injected 0 would be equally "fixed" and equally wrong, since 0 is the
    /// value the engines already defaulted to. And the array's own init must survive untouched —
    /// injecting a `constd` at an array sort emits ill-typed BTOR2.
    #[test]
    fn a_memory_init_no_longer_suppresses_reset_init_for_the_scalar_flops() {
        // `q` is async-reset to 5; `mem` is a memory with the init yosys always emits.
        let btor2 = "1 sort bitvec 1\n\
                     2 input 1 rst_n\n\
                     3 sort bitvec 4\n\
                     4 state 3 q\n\
                     5 const 3 0101\n\
                     6 input 3 d\n\
                     7 ite 3 2 6 5\n\
                     8 next 3 4 7\n\
                     9 sort bitvec 8\n\
                     10 sort array 3 9\n\
                     11 state 10 mem\n\
                     12 const 9 00000000\n\
                     13 init 10 11 12\n";

        let out = inject_reset_init(btor2, &[("rst_n".into(), 1)]).expect("array-bearing model");

        assert!(
            out.contains("init 3 4 "),
            "the scalar flop must get an init — one array init used to suppress all of them:\n{out}"
        );
        assert!(
            out.contains("constd 3 5"),
            "and it must be the RESET value 5, not the 0 the engines already default to:\n{out}"
        );
        assert_eq!(
            out.matches("init 10 11").count(),
            1,
            "the memory's own init is authoritative and must survive exactly once:\n{out}"
        );
        assert!(
            !out.contains("constd 10 "),
            "a constd at an ARRAY sort is ill-typed BTOR2 and must never be emitted:\n{out}"
        );
    }

    /// A cutpoint is free at EVERY cycle, not merely at cycle 0 — reset-init must leave it alone.
    ///
    /// `--cutpoint` and submodule blackboxing both produce a state cell with no `next`. Pinning its
    /// cycle-0 value removes start states, so — unlike the mununu#577 defect, which was unsound for
    /// VIOLATED — this direction would be unsound for **HOLDS**. Fixing one must not introduce the
    /// other, which is why the exclusion is asserted rather than left to the array filter.
    #[test]
    fn a_cutpoint_state_is_left_free_because_no_reset_reaches_it() {
        let btor2 = "1 sort bitvec 1\n\
                     2 input 1 rst_n\n\
                     3 sort bitvec 4\n\
                     4 state 3 q\n\
                     5 const 3 0101\n\
                     6 input 3 d\n\
                     7 ite 3 2 6 5\n\
                     8 next 3 4 7\n\
                     9 state 3 cutpoint_net\n";

        let out = inject_reset_init(btor2, &[("rst_n".into(), 1)]).expect("cutpoint model");

        assert!(
            out.contains("constd 3 5"),
            "the reset register is still established:\n{out}"
        );
        assert!(
            !out.contains("init 3 9 "),
            "the cutpoint has no `next`, so it is free by construction and must stay free:\n{out}"
        );
    }

    use super::*;

    /// Resolve a state cell's init value (mirrors how the exact engine's
    /// `initial_state_bdd` reads the BTOR2 init), for assertions.
    fn init_value(content: &str, state_symbol: &str) -> Option<u128> {
        let file = parser::parse(content).ok()?;
        let symbols = parser::collect_symbols(&file);
        let state_nid = file.lines.iter().find_map(|l| match &l.node {
            Node::State { .. } if symbols.get(&l.nid).map(String::as_str) == Some(state_symbol) => {
                Some(l.nid)
            }
            _ => None,
        })?;
        let value_op = file.lines.iter().find_map(|l| match &l.node {
            Node::Init { state, value, .. } if *state == state_nid => Some(*value),
            _ => None,
        })?;
        // The init value operand references a const line; resolve its value.
        file.lines.iter().find_map(|l| {
            if l.nid != value_op.nid() {
                return None;
            }
            match &l.node {
                Node::Const {
                    value: crate::adapter::btor2::ast::ConstValue::Dec(d),
                    ..
                } => Some(*d as u128),
                _ => None,
            }
        })
    }

    // Async-reset FSM, active-HIGH reset: `next(fsm) = ite(rst, 1, 2)` — asserting
    // rst forces the reset value 1. No `init` line (the async-reset shape). The
    // injection must init fsm at its reset value 1, NOT the power-on default 0.
    const ASYNC_RESET_HIGH: &str = r#"1 sort bitvec 1
2 sort bitvec 2
3 state 2 fsm
4 constd 2 1
5 constd 2 2
6 input 1 rst
7 ite 2 6 4 5
8 next 2 3 7
"#;

    #[test]
    fn injects_reset_value_as_init_active_high() {
        // Active-high reset ⇒ inactive level 0 ⇒ asserted level 1.
        let out = inject_reset_init(ASYNC_RESET_HIGH, &[("rst".to_string(), 0)]).unwrap();
        assert_eq!(
            init_value(&out, "fsm"),
            Some(1),
            "fsm must init at the reset value 1, not the power-on default 0; got:\n{out}"
        );
    }

    #[test]
    fn injects_reset_value_as_init_active_low() {
        // Active-low reset shape: `next(fsm) = ite(rst_ni, 2, 1)` — rst_ni LOW
        // (asserted) selects the reset value 1. Inactive level 1 ⇒ asserted 0.
        let src = r#"1 sort bitvec 1
2 sort bitvec 2
3 state 2 fsm
4 constd 2 1
5 constd 2 2
6 input 1 rst_ni
7 ite 2 6 5 4
8 next 2 3 7
"#;
        let out = inject_reset_init(src, &[("rst_ni".to_string(), 1)]).unwrap();
        assert_eq!(
            init_value(&out, "fsm"),
            Some(1),
            "fsm must init at the reset value 1 for an active-low reset; got:\n{out}"
        );
    }

    #[test]
    fn no_op_without_resets() {
        let out = inject_reset_init(ASYNC_RESET_HIGH, &[]).unwrap();
        assert_eq!(out, ASYNC_RESET_HIGH, "empty resets ⇒ unchanged");
    }

    #[test]
    fn no_op_when_init_already_present() {
        // A design with an authoritative BTOR2 init (fsm init = 2) must be left
        // untouched — the reset injection never overrides an explicit init.
        let src = r#"1 sort bitvec 1
2 sort bitvec 2
3 state 2 fsm
4 constd 2 1
5 constd 2 2
6 input 1 rst
7 ite 2 6 4 5
8 next 2 3 7
9 init 2 3 5
"#;
        let out = inject_reset_init(src, &[("rst".to_string(), 0)]).unwrap();
        assert_eq!(out, src, "existing init is authoritative ⇒ unchanged");
    }

    // A reset-less design with a PARTIAL initial: `st` (2-bit) carries an
    // authoritative `init 2`, but `stall` (1-bit) has none. `inject_zero_init`
    // must complete `stall` to 0 (matching the cube/exact power-up) while leaving
    // `st`'s init untouched — the wbicapetwo shape that caused the exact-vs-
    // portfolio disagreement.
    const PARTIAL_INITIAL: &str = r#"1 sort bitvec 1
2 sort bitvec 2
3 state 2 st
4 state 1 stall
5 constd 2 2
6 init 2 3 5
7 constd 2 0
8 next 2 3 7
9 constd 1 1
10 next 1 4 9
"#;

    #[test]
    fn zero_init_completes_initless_bitvec_cell() {
        let out = inject_zero_init(PARTIAL_INITIAL).unwrap();
        // `stall` gets an explicit 0 init; `st` keeps its authoritative 2.
        assert_eq!(init_value(&out, "stall"), Some(0), "init-less stall ⇒ 0");
        assert_eq!(
            init_value(&out, "st"),
            Some(2),
            "authoritative st untouched"
        );
    }

    #[test]
    fn zero_init_no_op_when_all_cells_initialised() {
        // Every state already has an init ⇒ unchanged.
        let src = r#"1 sort bitvec 1
2 state 1 q
3 constd 1 1
4 init 1 2 3
5 next 1 2 3
"#;
        assert_eq!(inject_zero_init(src).unwrap(), src, "all-init ⇒ unchanged");
    }

    #[test]
    fn zero_init_skips_array_sorted_state() {
        // An array-sorted state (a memory) must NOT get a `constd` init (ill-typed);
        // the bitvec `q` still gets its 0.
        let src = r#"1 sort bitvec 1
2 sort bitvec 8
3 sort array 1 2
4 state 3 mem
5 state 1 q
6 next 1 5 5
"#;
        let out = inject_zero_init(src).unwrap();
        assert_eq!(init_value(&out, "q"), Some(0), "bitvec q ⇒ 0");
        assert!(
            !out.contains("init 3 4"),
            "array-sorted mem must not be zero-inited"
        );
    }
}
