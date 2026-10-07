//! H.O.0 — internal exact oracle for verify-auto verdicts.
//!
//! The 2026-06-29 soundness review found we have **no independent check** that a
//! DEFINITE verify-auto verdict is correct — and a spurious `HOLDS` (silently
//! claiming a property holds) is more dangerous than a spurious `VIOLATED` and
//! nothing catches it. This module is the cheapest oracle: it computes a safety
//! property's **concrete** truth by bounded reachability enumeration (mununu's
//! own exact one-step semantics, no abstraction), so a cube-path `HOLDS` that the
//! concrete design contradicts is caught.
//!
//! **Fragment.** Two property shapes over state-register / 1-bit-input atoms:
//! - [`ag_state_atom`] — `AG (signal ⋈ value)` over a state register (or a
//!   value-alias of one, resolved like the verify-auto seeder; H.O.0.3).
//! - [`ag_implies_next`] — `AG (ante → AX cons)`, the SVA `ante |=> cons` shape,
//!   with a state / 1-bit-input antecedent and a state-register consequent
//!   (H.O.0.3).
//!
//! Combinational *functions* of state/input (which need full node evaluation)
//! are out of fragment and error rather than mis-bind. The reachability is
//! **sound for finding violations** always, and **sound for concluding AG-true**
//! only when the full input space was enumerated (`!bounded`); a `bounded`
//! result never concludes true.
//!
//! Not wired into the production path — a validation oracle, consumed by the
//! verify-auto differential + e2e tests (H.O.0.2 / H.O.0.3): `AgOracle` ⟷ the
//! cube verdict via `spurious_verdict`.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::adapter::AdapterError;
use crate::adapter::btor2::ast::{ConstValue, Node};
use crate::adapter::btor2::parser::{self, bv_width};
use crate::adapter::btor2::predicate_expr::CmpOp;
use crate::adapter::btor2::{ast::Btor2File, bit_blast};

/// Bounded reachable set of concrete register valuations.
#[derive(Debug, Clone)]
pub struct Reachability {
    /// Reachable register-state valuations (keyed by state-cell symbol).
    pub states: Vec<BTreeMap<String, u128>>,
    /// True when enumeration was capped (by `max_states`) or some input could
    /// not be exhaustively enumerated (wider than `max_input_bits`). A bounded
    /// reachability can still witness a violation, but cannot conclude AG-true.
    pub bounded: bool,
}

/// The concrete `AG (atom)` verdict from bounded reachability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgOracle {
    /// The atom holds at every reachable state AND the full input space was
    /// enumerated — `AG atom` is concretely TRUE.
    Holds,
    /// A reachable state violates the atom — `AG atom` is concretely FALSE.
    /// Carries the witness register valuation.
    Violated(BTreeMap<String, u128>),
    /// No violation found, but enumeration was bounded — inconclusive for TRUE
    /// (the oracle can refute a cube-HOLDS but not confirm it here).
    Inconclusive,
}

/// Init value of each state cell that HAS a BTOR2 `init` line with a constant value.
///
/// A state with no `init` line is **free at cycle 0** — every value is a real initial state
/// (the BTOR2 format; mununu#505 fixed the exact engine's reading of it). It is therefore
/// ABSENT from this map, never defaulted. Defaulting it to 0 pinned the cube paths'
/// "reset cube" to one of many initial states, which fabricated a definite `Holds` on
/// `AG EF (done == 1)` over two held registers with free initial values (found
/// 2026-10-06 while calibrating profiling cases: the exact engine said VIOLATED, the
/// cube escalation said HOLDS). See [`init_valuation_defaulted`] for the simulation
/// seed that still needs a full valuation.
///
/// `pub(crate)` (P2 Slice 1): the recoverability and safety cube paths read it to build
/// the initial cube; a predicate over a register absent here is enumerated over both
/// truth values there, not pinned.
pub(crate) fn init_valuation(file: &Btor2File) -> BTreeMap<String, u128> {
    let symbols = parser::collect_symbols(file);
    let mut init_of: HashMap<crate::adapter::btor2::ast::Nid, u128> = HashMap::new();
    for line in &file.lines {
        if let Node::Init { state, value, .. } = &line.node
            && let Some(cl) = file.lookup(value.nid())
            && let Node::Const { sort, value: cv } = &cl.node
        {
            init_of.insert(*state, const_to_u128(file, *sort, cv));
        }
    }
    let mut out = BTreeMap::new();
    for line in &file.lines {
        if matches!(line.node, Node::State { .. })
            && let Some(name) = symbols.get(&line.nid)
            && let Some(v) = init_of.get(&line.nid)
        {
            out.insert(name.clone(), *v);
        }
    }
    out
}

/// Every state cell with a value: the `init`-line value where there is one, 0 otherwise —
/// the `setundef -zero` power-up — plus whether any cell was defaulted. ONLY for seeding a
/// concrete simulation that needs a total valuation; a caller that gets `defaulted == true`
/// explored ONE of the design's initial states and must not claim anything about all of them.
pub(crate) fn init_valuation_defaulted(file: &Btor2File) -> (BTreeMap<String, u128>, bool) {
    let symbols = parser::collect_symbols(file);
    let pinned = init_valuation(file);
    let mut out = BTreeMap::new();
    let mut defaulted = false;
    for line in &file.lines {
        if matches!(line.node, Node::State { .. })
            && let Some(name) = symbols.get(&line.nid)
        {
            match pinned.get(name) {
                Some(v) => {
                    out.insert(name.clone(), *v);
                }
                None => {
                    defaulted = true;
                    out.insert(name.clone(), 0);
                }
            }
        }
    }
    (out, defaulted)
}

fn const_to_u128(file: &Btor2File, sort: crate::adapter::btor2::ast::Nid, cv: &ConstValue) -> u128 {
    match cv {
        ConstValue::Zero => 0,
        ConstValue::One => 1,
        ConstValue::Ones => {
            let w = bv_width(file, sort).unwrap_or(0);
            if w == 0 || w >= 128 {
                u128::MAX
            } else {
                (1u128 << w) - 1
            }
        }
        ConstValue::Dec(d) => *d as u128,
        ConstValue::Bin(b) => u128::from_str_radix(b, 2).unwrap_or(0),
        ConstValue::Hex(h) => u128::from_str_radix(h, 16).unwrap_or(0),
    }
}

/// 1-bit primary input symbols (the enumerable environment).
fn boolean_inputs(file: &Btor2File) -> (Vec<String>, bool) {
    let symbols = parser::collect_symbols(file);
    let mut bool_ins = Vec::new();
    let mut has_wide = false;
    for line in &file.lines {
        if let Node::Input { sort, .. } = &line.node {
            let w = bv_width(file, *sort).unwrap_or(0);
            match (w, symbols.get(&line.nid)) {
                (1, Some(name)) => bool_ins.push(name.clone()),
                // a wide (>1-bit) input we will NOT exhaustively enumerate →
                // reachability becomes bounded (sound only for finding violations).
                _ => has_wide = true,
            }
        }
    }
    bool_ins.sort();
    (bool_ins, has_wide)
}

/// BFS the reachable concrete register-states from init, simulating one step over
/// every combination of the (1-bit) primary inputs. Bounded by `max_states`; a
/// wide input or hitting the cap sets `bounded`.
pub fn reachable_register_states(
    file: &Btor2File,
    max_states: usize,
    max_input_bits: usize,
) -> Result<Reachability, AdapterError> {
    let (bool_ins, has_wide) = boolean_inputs(file);
    let n_in = bool_ins.len().min(max_input_bits);
    // bounded if a wide input exists, or we capped the input bits we enumerate.
    let mut bounded = has_wide || bool_ins.len() > max_input_bits;
    let n_combos: usize = 1usize << n_in;

    // A free-init register is explored from ONE of its values, so the enumeration is
    // bounded (incomplete) by construction: `Holds` must not be claimed from it.
    let (init, defaulted) = init_valuation_defaulted(file);
    if defaulted {
        bounded = true;
    }
    let mut seen: HashSet<Vec<(String, u128)>> = HashSet::new();
    // `order` doubles as the BFS queue (walked by `head`) and the result list.
    let mut order: Vec<BTreeMap<String, u128>> = Vec::new();
    let key = |s: &BTreeMap<String, u128>| -> Vec<(String, u128)> {
        s.iter().map(|(k, v)| (k.clone(), *v)).collect()
    };
    seen.insert(key(&init));
    order.push(init);

    let mut head = 0;
    while head < order.len() {
        if order.len() >= max_states {
            bounded = true;
            break;
        }
        let state = order[head].clone();
        head += 1;
        let regs: HashMap<String, u128> = state.iter().map(|(k, v)| (k.clone(), *v)).collect();
        for combo in 0..n_combos {
            let inputs: HashMap<String, u128> = bool_ins
                .iter()
                .take(n_in)
                .enumerate()
                .map(|(b, name)| (name.clone(), ((combo >> b) & 1) as u128))
                .collect();
            let next = bit_blast::simulate_one_step(file, &regs, &inputs)?;
            let next_sorted: BTreeMap<String, u128> = next.into_iter().collect();
            if seen.insert(key(&next_sorted)) {
                if order.len() < max_states {
                    order.push(next_sorted);
                } else {
                    bounded = true;
                }
            }
        }
    }
    Ok(Reachability {
        states: order,
        bounded,
    })
}

fn cmp_holds(op: CmpOp, lhs: u128, rhs: u128) -> bool {
    match op {
        CmpOp::Eq => lhs == rhs,
        CmpOp::Ne => lhs != rhs,
        CmpOp::Lt => lhs < rhs,
        CmpOp::Le => lhs <= rhs,
        CmpOp::Gt => lhs > rhs,
        CmpOp::Ge => lhs >= rhs,
    }
}

/// H.O.0.3 — resolve a user-facing signal name to the canonical state-cell
/// SYMBOL the reachability maps key against. Mirrors how the verify-auto seeder
/// binds (`resolve_state_alias`, the strict value-alias resolver): a direct
/// state symbol resolves to itself; a `uext`/`sext`-by-0 or `output` rename of a
/// state cell resolves to that cell; a combinational *function* of state does
/// NOT resolve (binding it to a state's value would read the wrong value).
///
/// `allow_reset_mux` (H.O.0.3b) controls whether an async-reset register mux
/// `ite(rst, state, resetval)` is followed to its state branch. Pass `true`
/// **only when the design's reset input is pinned inactive** (e.g. the caller
/// ran `pin_inputs_to_constants` to match verify_auto's reset-gating): then the
/// mux always selects the state branch, so the register's value equals the
/// state's and binding is sound — the same condition under which the verify-auto
/// seeder follows it. Pass `false` when reset is a free input (the register only
/// equals the state while reset is inactive, which free enumeration breaks).
fn resolve_signal_symbol(file: &Btor2File, signal: &str, allow_reset_mux: bool) -> Option<String> {
    // AR-GO-1 — the STRICT (value-identical-alias) resolution, made explicit.
    parser::resolve_to_canonical_name(
        file,
        signal,
        parser::ResolveStrictness::Strict { allow_reset_mux },
    )
}

/// Concrete `AG (signal ⋈ value)` oracle over a **state-register** signal (or a
/// value-alias of one — H.O.0.3 resolution). Reads each reachable state's
/// register value directly — no abstraction. A signal that does not resolve to a
/// state cell returns an error (out of the AG-state-atom fragment). See
/// [`resolve_signal_symbol`] for `reset_pinned`.
pub fn ag_state_atom(
    file: &Btor2File,
    signal: &str,
    op: CmpOp,
    value: u128,
    max_states: usize,
    max_input_bits: usize,
    reset_pinned: bool,
) -> Result<AgOracle, AdapterError> {
    let cell = resolve_signal_symbol(file, signal, reset_pinned).ok_or_else(|| AdapterError {
        kind: crate::adapter::AdapterErrorKind::IrConsistencyError,
        location: None,
        message: format!(
            "concrete_oracle::ag_state_atom: `{signal}` does not resolve to a state cell \
             (the AG(state-atom) fragment requires a state register or a value-alias of one)"
        ),
    })?;
    let reach = reachable_register_states(file, max_states, max_input_bits)?;
    for st in &reach.states {
        let v = st.get(&cell).copied().unwrap_or(0);
        if !cmp_holds(op, v, value) {
            return Ok(AgOracle::Violated(st.clone()));
        }
    }
    if reach.bounded {
        Ok(AgOracle::Inconclusive)
    } else {
        Ok(AgOracle::Holds)
    }
}

/// One `signal ⋈ value` comparison atom over a state register or a primary
/// input — the building block of the `|=>` fragment (H.O.0.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleAtom {
    pub signal: String,
    pub op: CmpOp,
    pub value: u128,
}

impl OracleAtom {
    pub fn new(signal: impl Into<String>, op: CmpOp, value: u128) -> Self {
        Self {
            signal: signal.into(),
            op,
            value,
        }
    }
}

/// Concrete `AG (ante → AX cons)` oracle — the SVA `ante |=> cons` shape, where
/// the consequent is checked one cycle later, at every successor.
///
/// `ante` may be a **state register** (value-alias resolved) OR a **1-bit
/// primary input** (read from the current cycle's input combination). `cons`
/// must be a **state register** — it is evaluated at the NEXT state. Concrete,
/// no abstraction: at every reachable state, for every enumerated 1-bit input
/// combination, if the antecedent holds then every one-step successor must
/// satisfy the consequent.
///
/// - `Violated(s)` — a reachable state `s` whose successor breaks the consequent
///   (a real concrete counterexample, sound even under a bounded enumeration).
/// - `Holds` — full enumeration, no break.
/// - `Inconclusive` — enumeration was bounded (wide inputs / cap) and no break
///   was found, so AG-true cannot be concluded.
///
/// Out-of-fragment signals error: a consequent that is not a state cell, or an
/// antecedent that is neither a state cell nor a 1-bit primary input (e.g. a
/// combinational function of state, deferred to a later increment). See
/// [`resolve_signal_symbol`] for `reset_pinned`.
pub fn ag_implies_next(
    file: &Btor2File,
    ante: &OracleAtom,
    cons: &OracleAtom,
    max_states: usize,
    max_input_bits: usize,
    reset_pinned: bool,
) -> Result<AgOracle, AdapterError> {
    let err = |m: String| AdapterError {
        kind: crate::adapter::AdapterErrorKind::IrConsistencyError,
        location: None,
        message: m,
    };
    // Consequent: a state cell (checked at the next state).
    let cons_cell = resolve_signal_symbol(file, &cons.signal, reset_pinned).ok_or_else(|| {
        err(format!(
            "concrete_oracle::ag_implies_next: consequent `{}` does not resolve to a state cell",
            cons.signal
        ))
    })?;
    // Antecedent: a state cell OR a 1-bit primary input.
    let (bool_ins, _has_wide) = boolean_inputs(file);
    let ante_cell = resolve_signal_symbol(file, &ante.signal, reset_pinned);
    let ante_is_input = bool_ins.iter().any(|n| n == &ante.signal);
    if ante_cell.is_none() && !ante_is_input {
        return Err(err(format!(
            "concrete_oracle::ag_implies_next: antecedent `{}` is neither a state cell nor a \
             1-bit primary input (out of the |=> fragment)",
            ante.signal
        )));
    }

    let reach = reachable_register_states(file, max_states, max_input_bits)?;
    let n_in = bool_ins.len().min(max_input_bits);
    let n_combos: usize = 1usize << n_in;

    for st in &reach.states {
        let regs: HashMap<String, u128> = st.iter().map(|(k, v)| (k.clone(), *v)).collect();
        for combo in 0..n_combos {
            let inputs: HashMap<String, u128> = bool_ins
                .iter()
                .take(n_in)
                .enumerate()
                .map(|(b, name)| (name.clone(), ((combo >> b) & 1) as u128))
                .collect();
            // Antecedent value at (state, inputs).
            let ante_val = match &ante_cell {
                Some(cell) => regs.get(cell).copied().unwrap_or(0),
                None => inputs.get(&ante.signal).copied().unwrap_or(0),
            };
            if !cmp_holds(ante.op, ante_val, ante.value) {
                continue;
            }
            // Consequent at the NEXT state (this specific input assignment).
            let next = bit_blast::simulate_one_step(file, &regs, &inputs)?;
            let cons_val = next.get(&cons_cell).copied().unwrap_or(0);
            if !cmp_holds(cons.op, cons_val, cons.value) {
                return Ok(AgOracle::Violated(st.clone()));
            }
        }
    }
    if reach.bounded {
        Ok(AgOracle::Inconclusive)
    } else {
        Ok(AgOracle::Holds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::parser::parse;

    // A 2-bit FSM cycling 0→1→2→0 (caps at 2, never reaches 3), input-free in
    // the next-state (deterministic). `state` is a 2-bit register.
    //   next(state) = (state == 2) ? 0 : state + 1
    // BTOR2: 1 sort2, 2 state, consts, the ite chain.
    const CYCLE_FSM: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 state 2 state
4 zero 2
5 one 2
6 constd 2 2
7 eq 1 3 6
8 add 2 3 5
9 ite 2 7 4 8
10 next 2 3 9
11 init 2 3 4
";

    /// A state with no `init` line is free at cycle 0: absent from the pinned valuation, defaulted
    /// (and flagged) only in the simulation seed, and a BFS from that seed is `bounded`.
    #[test]
    fn free_init_state_is_absent_from_the_pinned_valuation_and_bounds_the_reach() {
        let src = "1 sort bitvec 1\n2 sort bitvec 2\n3 state 2 x\n4 next 2 3 3\n5 state 1 y\n6 zero 1\n7 init 1 5 6\n8 next 1 5 5\n";
        let file = parser::parse(src).expect("parse");
        let pinned = init_valuation(&file);
        assert_eq!(pinned.get("y"), Some(&0));
        assert!(
            !pinned.contains_key("x"),
            "x has no init line and must not be defaulted to 0"
        );
        let (seed, defaulted) = init_valuation_defaulted(&file);
        assert_eq!(seed.get("x"), Some(&0));
        assert!(defaulted);
        let r = reachable_register_states(&file, 64, 8).expect("reach");
        assert!(
            r.bounded,
            "a reach from a defaulted free-init seed explores one initial state of many"
        );
    }

    #[test]
    fn reachability_enumerates_the_cycle() {
        let file = parse(CYCLE_FSM).expect("parse cycle fsm");
        let r = reachable_register_states(&file, 64, 8).expect("reach");
        let vals: std::collections::HashSet<u128> = r.states.iter().map(|s| s["state"]).collect();
        // reachable states: 0, 1, 2 (never 3). Fully enumerated (no inputs).
        assert_eq!(
            vals,
            [0, 1, 2].into_iter().collect(),
            "0→1→2→0 cycle, 3 unreachable"
        );
        assert!(!r.bounded, "no inputs ⇒ full enumeration");
    }

    #[test]
    fn ag_holds_for_unreachable_value() {
        let file = parse(CYCLE_FSM).expect("parse");
        // AG (state != 3) — 3 is unreachable → HOLDS (full enumeration).
        assert_eq!(
            ag_state_atom(&file, "state", CmpOp::Ne, 3, 64, 8, false).unwrap(),
            AgOracle::Holds
        );
    }

    #[test]
    fn ag_violated_for_reachable_value() {
        let file = parse(CYCLE_FSM).expect("parse");
        // AG (state != 1) — 1 IS reachable → VIOLATED, with the witness.
        match ag_state_atom(&file, "state", CmpOp::Ne, 1, 64, 8, false).unwrap() {
            AgOracle::Violated(st) => assert_eq!(st["state"], 1),
            other => panic!("expected Violated(state=1), got {other:?}"),
        }
    }

    #[test]
    fn ag_le_bound_holds() {
        let file = parse(CYCLE_FSM).expect("parse");
        // AG (state <= 2) — every reachable state ≤ 2 → HOLDS.
        assert_eq!(
            ag_state_atom(&file, "state", CmpOp::Le, 2, 64, 8, false).unwrap(),
            AgOracle::Holds
        );
    }

    #[test]
    fn non_state_signal_is_rejected() {
        let file = parse(CYCLE_FSM).expect("parse");
        assert!(ag_state_atom(&file, "not_a_state", CmpOp::Eq, 0, 64, 8, false).is_err());
    }

    // ---- H.O.0.3 — `|=>` (AX) oracle + signal resolution -------------------

    // A 1-bit FSM `q` with a 1-bit input `en`: next(q) = en ? 1 : q. Reset 0.
    // Reachable {0,1}; `en` high forces q to 1 next cycle.
    const GATED_FSM: &str = "\
1 sort bitvec 1
2 state 1 q
3 input 1 en
4 one 1
5 ite 1 3 4 2
6 next 1 2 5
7 zero 1
8 init 1 2 7
";

    // A 2-bit state `st_internal` stuck at 0, exposed under the value-alias name
    // `st_alias` (a `uext`-by-0 rename — what Yosys' flatten emits).
    const ALIAS_FSM: &str = "\
1 sort bitvec 2
2 state 1 st_internal
3 zero 1
4 next 1 2 3
5 init 1 2 3
6 uext 1 2 0 st_alias
";

    #[test]
    fn ag_implies_next_holds() {
        let file = parse(CYCLE_FSM).expect("parse");
        // AG (state == 0 |=> state == 1) — from 0 the next state is always 1.
        let ante = OracleAtom::new("state", CmpOp::Eq, 0);
        let cons = OracleAtom::new("state", CmpOp::Eq, 1);
        assert_eq!(
            ag_implies_next(&file, &ante, &cons, 64, 8, false).unwrap(),
            AgOracle::Holds
        );
    }

    #[test]
    fn ag_implies_next_violated() {
        let file = parse(CYCLE_FSM).expect("parse");
        // AG (state == 0 |=> state == 2) — from 0 the next is 1, not 2 → VIOLATED.
        let ante = OracleAtom::new("state", CmpOp::Eq, 0);
        let cons = OracleAtom::new("state", CmpOp::Eq, 2);
        match ag_implies_next(&file, &ante, &cons, 64, 8, false).unwrap() {
            AgOracle::Violated(st) => assert_eq!(st["state"], 0),
            other => panic!("expected Violated(state=0), got {other:?}"),
        }
    }

    #[test]
    fn ag_implies_next_input_antecedent_holds() {
        let file = parse(GATED_FSM).expect("parse");
        // AG (en |=> q == 1) — whenever `en` is high, q is 1 next cycle.
        let ante = OracleAtom::new("en", CmpOp::Eq, 1);
        let cons = OracleAtom::new("q", CmpOp::Eq, 1);
        assert_eq!(
            ag_implies_next(&file, &ante, &cons, 64, 8, false).unwrap(),
            AgOracle::Holds
        );
    }

    #[test]
    fn ag_implies_next_input_antecedent_violated() {
        let file = parse(GATED_FSM).expect("parse");
        // AG (en |=> q == 0) — `en` high forces q to 1, contradicting q == 0.
        let ante = OracleAtom::new("en", CmpOp::Eq, 1);
        let cons = OracleAtom::new("q", CmpOp::Eq, 0);
        assert!(matches!(
            ag_implies_next(&file, &ante, &cons, 64, 8, false).unwrap(),
            AgOracle::Violated(_)
        ));
    }

    #[test]
    fn ag_implies_next_rejects_input_consequent() {
        let file = parse(GATED_FSM).expect("parse");
        // The consequent must be a state cell; an input consequent is out of
        // the |=> fragment (it has no "next state" value).
        let ante = OracleAtom::new("q", CmpOp::Eq, 0);
        let cons = OracleAtom::new("en", CmpOp::Eq, 1);
        assert!(ag_implies_next(&file, &ante, &cons, 64, 8, false).is_err());
    }

    #[test]
    fn resolve_signal_follows_value_alias() {
        let file = parse(ALIAS_FSM).expect("parse");
        // `st_alias` is a uext-0 rename of the state `st_internal` (stuck at 0).
        // The oracle binds it to the underlying cell and reads its real value.
        assert_eq!(
            resolve_signal_symbol(&file, "st_alias", false).as_deref(),
            Some("st_internal")
        );
        assert_eq!(
            ag_state_atom(&file, "st_alias", CmpOp::Eq, 0, 64, 8, false).unwrap(),
            AgOracle::Holds
        );
    }
}
