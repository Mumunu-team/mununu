//! **Trap certificate** — decide a recoverability row `AG EF p` VIOLATED by exhibiting a reachable
//! set of states that is closed under every transition and avoids `p`.
//!
//! # The certificate
//!
//! `AG EF p` is false iff some reachable state satisfies `AG ¬p`. A predicate `T` certifies that
//! when
//!
//! 1. **reach** — `EF T`: some state in `T` is reachable from reset (a witness path);
//! 2. **closure** — `AG (T → [] T)`: every successor of every reachable `T`-state is in `T`;
//! 3. **disjointness** — `T → ¬p`.
//!
//! From the reachable `T`-state of (1) every path stays in `T` by (2), so `p` never holds again by
//! (3): `EF p` fails there, and `AG EF p` is VIOLATED. Closure over the *reachable* states suffices,
//! because every successor of a reachable state is itself reachable. Both (1) and (2) are safety
//! questions on the model as given, so nothing is approximated: (2) is the `|=>` monitor
//! `bad = T(prev) ∧ ¬T` ([`emit_ag_implies_next_compound_monitor`]), (1) is the dual `AG ¬T` of the
//! reachability twin. (3) is syntactic: `T` and `p` are `R == v` and `R == w` over the same signal
//! with `v ≠ w`. **The certificate only ever refutes**: a failed search leaves the row as it was.
//!
//! `T` is one register value. Candidates come from two places, cheapest first:
//!
//! - **sibling rows** — a row in the same report that already HOLDS the terminal idiom
//!   `AG (R == v → AG R == v)` ([`reduce_terminal_state`]). The sibling only orders the search: the
//!   closure is re-checked here, so a stale sibling verdict can never become a certificate.
//! - **the register's value domain** — the enum values of `R`'s state cell
//!   ([`detect_enum_states`]), else the constants it is initialised to, compared against or assigned
//!   ([`legal_encodings`]).
//!
//! # When it is withheld
//!
//! Reachability (1) transfers to the RTL only when the model is not an over-approximation in `R`'s
//! cone. A free leaf in the sequential cone of `R` — an undriven net (an anonymous BTOR2 `input`) or
//! a state with no `next` (a `--cutpoint`, a black-boxed output) — can drive the model into `T`
//! along a path the real driver never takes. The search is then not run and the caller reports the
//! leaves; closure alone would transfer, reachability would not.
//!
//! engine: closure — `native` k-induction (z3 QF_BV; a genuine trap is 1-inductive) as the decider
//! inside the reachability portfolio ([`decide_reach_portfolio`]: exact ROBDD under its cap,
//! interpolation, btormc / pono / rIC3 when installed; SPACER corroborates only), one portfolio call
//! per candidate under a per-check budget; reach — the same portfolio on the dual `AG ¬T`, then
//! `native` BMC ([`safety_witness`]) for the shallowest reset → `T` path. Role: decider for
//! VIOLATED only.
//!
//! [`emit_ag_implies_next_compound_monitor`]: crate::adapter::btor2::bad_monitor::emit_ag_implies_next_compound_monitor
//! [`reduce_terminal_state`]: crate::adapter::liveness_rescue::reduce_terminal_state
//! [`detect_enum_states`]: crate::adapter::btor2::bit_blast::detect_enum_states
//! [`legal_encodings`]: crate::adapter::fsm_scan::legal_encodings
//! [`decide_reach_portfolio`]: crate::adapter::reach_portfolio::decide_reach_portfolio
//! [`safety_witness`]: crate::adapter::reach_rescue::safety_witness

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::adapter::btor2::ast::{Btor2File, Nid, Node, Operand};
use crate::adapter::btor2::native_bmc::BmcTrace;
use crate::adapter::btor2::parser;
use crate::adapter::btor2::predicate_expr::{CmpOp, PredicateExpr, parse_predicate_atom_bool};
use crate::adapter::liveness_rescue::Atom;
use crate::adapter::reach_rescue::{RescueVerdict, reach_portfolio_rescue_diagnosed};
use crate::adapter::run_budget;
use crate::mu_calculus::parser as mu_parser;
use crate::verdict::PropertyVerdict;

/// At most this many candidate values are tried per row (siblings first). A register with more
/// legal values than this is a datapath, not a control FSM, and its traps are not values.
pub const MAX_TRAP_CANDIDATES: usize = 16;

/// The budget of ONE check (a closure or a reach query on one candidate), a child of the caller's
/// budget. A genuine trap is 1-inductive and decides in milliseconds; a candidate whose closure
/// needs an invariant, or whose unreachability is hard, is given up on rather than allowed to eat
/// the row's slice.
pub const TRAP_CHECK_MS: u64 = 10_000;

/// The deepest reset → `T` path the witness BMC unrolls to (the bound of the safety witnesses).
const WITNESS_MAX_K: u32 = 60;

/// Where a candidate came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrapSource {
    /// A row of the same report that HOLDS `AG (T → AG T)` (its name).
    Sibling(String),
    /// The register's value domain.
    Domain,
}

impl TrapSource {
    /// The short provenance label (`sibling:<row>` / `enum-domain`).
    pub fn label(&self) -> String {
        match self {
            TrapSource::Sibling(row) => format!("sibling:{row}"),
            TrapSource::Domain => "enum-domain".to_string(),
        }
    }
}

/// One candidate trap `signal == value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrapCandidate {
    pub value: u64,
    pub source: TrapSource,
}

/// What the search established about one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateOutcome {
    /// Reachable and closed: the certificate.
    Certified,
    /// Some reachable `T`-state has a successor outside `T`.
    ReachableButOpen,
    /// Closed, but no `T`-state is reachable from reset.
    ClosedButUnreachable,
    /// The closure query did not decide within its budget.
    ClosureUndecided,
    /// Closed, and the reach query did not decide within its budget.
    ReachUndecided,
    /// Two sound portfolio members disagreed on a query — a soundness alarm, never a certificate.
    Contradiction,
    /// A monitor could not be built for this candidate (the reason).
    Declined(String),
}

impl CandidateOutcome {
    /// The kebab-case label the search note lists.
    pub fn tag(&self) -> String {
        match self {
            CandidateOutcome::Certified => "certified".into(),
            CandidateOutcome::ReachableButOpen => "reachable-but-open".into(),
            CandidateOutcome::ClosedButUnreachable => "closed-but-unreachable".into(),
            CandidateOutcome::ClosureUndecided => "closure-undecided".into(),
            CandidateOutcome::ReachUndecided => "reach-undecided".into(),
            CandidateOutcome::Contradiction => "contradiction".into(),
            CandidateOutcome::Declined(why) => format!("declined ({why})"),
        }
    }
}

/// A certified trap: `signal == value` is reachable and closed, and the target is outside it.
#[derive(Debug, Clone)]
pub struct TrapCertificate {
    pub signal: String,
    pub value: u64,
    pub source: TrapSource,
    /// The engines that proved the closure monitor's `bad` unreachable.
    pub closure_engines: Vec<&'static str>,
    /// The engines that reached `T` (the dual's `bad`).
    pub reach_engines: Vec<&'static str>,
    /// The shallowest reset → `T` path native BMC found (`None` when `T` is deeper than the bound;
    /// the certificate stands on the portfolio's reach verdict then).
    pub witness: Option<BmcTrace>,
}

/// The result of a search over one row's candidates.
#[derive(Debug, Clone, Default)]
pub struct TrapSearch {
    pub certificate: Option<TrapCertificate>,
    /// Every candidate tried, in order, with what was established.
    pub tried: Vec<(TrapCandidate, CandidateOutcome)>,
    /// Non-empty when the search was withheld: the free leaves (undriven nets, next-less states) in
    /// the target register's sequential cone.
    pub withheld_by: Vec<String>,
}

/// The candidate traps for `AG EF good`, siblings first, then the register's value domain;
/// deduplicated, `good`'s own value excluded, at most [`MAX_TRAP_CANDIDATES`].
///
/// `siblings` are `(row name, T)` pairs harvested from rows that HOLD the terminal idiom; only those
/// over `good`'s own signal with an `==` operator are used.
pub fn trap_candidates(
    btor2: &str,
    good: &Atom,
    reset_pinned: bool,
    siblings: &[(String, Atom)],
) -> Vec<TrapCandidate> {
    let mut out: Vec<TrapCandidate> = Vec::new();
    let mut seen: HashSet<u64> = HashSet::new();
    let mut push = |value: u128, source: TrapSource, out: &mut Vec<TrapCandidate>| {
        let Ok(value) = u64::try_from(value) else {
            return;
        };
        if u128::from(value) != good.value && seen.insert(value) {
            out.push(TrapCandidate { value, source });
        }
    };
    if good.op != CmpOp::Eq {
        return out;
    }
    for (row, t) in siblings {
        if t.signal == good.signal && t.op == CmpOp::Eq {
            push(t.value, TrapSource::Sibling(row.clone()), &mut out);
        }
    }
    if let Ok(file) = parser::parse(btor2)
        && let Some(nid) = parser::resolve_state_alias(&file, &good.signal, reset_pinned)
    {
        for v in register_domain(&file, nid) {
            push(u128::from(v), TrapSource::Domain, &mut out);
        }
    }
    out.truncate(MAX_TRAP_CANDIDATES);
    out
}

/// The values a state cell can hold, as far as the netlist says: its enum domain when the
/// next-state value path is a mux of constants ([`detect_enum_states`]), else the constants it is
/// initialised to, compared against or assigned ([`legal_encodings`]).
///
/// [`detect_enum_states`]: crate::adapter::btor2::bit_blast::detect_enum_states
/// [`legal_encodings`]: crate::adapter::fsm_scan::legal_encodings
fn register_domain(file: &Btor2File, nid: Nid) -> BTreeSet<u64> {
    if let Some(meta) = crate::adapter::btor2::bit_blast::detect_enum_states(file)
        .into_iter()
        .find(|m| m.nid == nid)
    {
        return meta.values.into_iter().collect();
    }
    let symbols = parser::collect_symbols(file);
    crate::adapter::fsm_scan::legal_encodings(file, nid, &symbols)
}

/// The free leaves in the sequential cone of `signal`: anonymous inputs (undriven nets) and states
/// with no `next` (cut points, black-boxed outputs). Rendered `input_<nid>` / the state's symbol.
/// Named primary inputs are the environment, not a free leaf. `None` when `signal` names nothing in
/// the file.
pub fn free_leaves_in_cone(file: &Btor2File, signal: &str) -> Option<Vec<String>> {
    leaves_in_cone(file, signal, true)
}

/// The undriven nets (anonymous BTOR2 `input`s, rendered `input_<nid>`) in the sequential cone of
/// `signal` — the free leaves nobody asked for: an unconnected wire, or the output of a module
/// missing from the source set. Unlike [`free_leaves_in_cone`] a deliberate cut (a `--cutpoint`,
/// a `(* blackbox *)` stub — a state with no `next`) is not counted. `None` when `signal` names
/// nothing in the file.
pub fn undriven_nets_in_cone(file: &Btor2File, signal: &str) -> Option<Vec<String>> {
    leaves_in_cone(file, signal, false)
}

fn leaves_in_cone(file: &Btor2File, signal: &str, with_cuts: bool) -> Option<Vec<String>> {
    let start = file.lines.iter().find_map(|l| match &l.node {
        Node::State {
            symbol: Some(s), ..
        }
        | Node::Input {
            symbol: Some(s), ..
        }
        | Node::Op {
            symbol: Some(s), ..
        } if s == signal => Some(l.nid),
        Node::Output {
            signal: op,
            symbol: Some(s),
        } if s == signal => Some(op.nid()),
        _ => None,
    })?;
    let next_of: HashMap<Nid, Operand> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Next { state, value, .. } => Some((*state, *value)),
            _ => None,
        })
        .collect();
    let init_of: HashMap<Nid, Operand> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Init { state, value, .. } => Some((*state, *value)),
            _ => None,
        })
        .collect();
    let mut leaves = BTreeSet::new();
    let mut visited: HashSet<Nid> = HashSet::new();
    let mut stack = vec![start];
    while let Some(nid) = stack.pop() {
        if !visited.insert(nid) {
            continue;
        }
        let Some(line) = file.lookup(nid) else {
            continue;
        };
        match &line.node {
            Node::Input { symbol: None, .. } => {
                leaves.insert(format!("input_{nid}"));
            }
            Node::State { symbol, .. } => {
                match next_of.get(&nid) {
                    Some(v) => stack.push(v.nid()),
                    None if with_cuts => {
                        leaves.insert(symbol.clone().unwrap_or_else(|| format!("state_{nid}")));
                    }
                    None => {}
                }
                if let Some(v) = init_of.get(&nid) {
                    stack.push(v.nid());
                }
            }
            Node::Op { args, .. } => stack.extend(args.iter().map(Operand::nid)),
            _ => {}
        }
    }
    Some(leaves.into_iter().collect())
}

/// Search `candidates` for a certified trap for `AG EF good`, in order, stopping at the first.
///
/// Each check runs under a child budget of at most [`TRAP_CHECK_MS`]; the search stops early when
/// the caller's budget expires. Withheld (no check run) when `good`'s register has a free leaf in
/// its sequential cone ([`free_leaves_in_cone`]).
pub fn search_trap(
    btor2: &str,
    good: &Atom,
    candidates: &[TrapCandidate],
    reset_pinned: bool,
) -> TrapSearch {
    let mut search = TrapSearch::default();
    if candidates.is_empty() || good.op != CmpOp::Eq {
        return search;
    }
    let Ok(file) = parser::parse(btor2) else {
        return search;
    };
    // SOUNDNESS: reachability of `T` on an over-approximated cone does not transfer to the RTL (a
    // free leaf can drive the model into `T` along a path the real driver never takes), so the
    // certificate is not attempted there. Closure would transfer; a refutation needs both.
    match free_leaves_in_cone(&file, &good.signal) {
        Some(leaves) if !leaves.is_empty() => {
            search.withheld_by = leaves;
            return search;
        }
        Some(_) => {}
        None => return search,
    }
    for cand in candidates {
        if run_budget::expired() {
            break;
        }
        let (outcome, cert) = check_candidate(btor2, &good.signal, cand, reset_pinned);
        search.tried.push((cand.clone(), outcome));
        if let Some(cert) = cert {
            search.certificate = Some(cert);
            break;
        }
    }
    search
}

/// One candidate: closure first (cheap for a genuine trap), reach only for a closed one.
fn check_candidate(
    btor2: &str,
    signal: &str,
    cand: &TrapCandidate,
    reset_pinned: bool,
) -> (CandidateOutcome, Option<TrapCertificate>) {
    let t = format!("({signal} == {})", cand.value);
    // (2) closure: AG (T → [] T) — the `|=>` monitor `bad = T(prev) ∧ ¬T`.
    let closure = match mu_parser::parse(&format!("nu X. ((!{t} || [] {t}) && [] X)")) {
        Ok(f) => f,
        Err(e) => return (CandidateOutcome::Declined(format!("{e:?}")), None),
    };
    let closure_engines = match under_check_budget(|| {
        reach_portfolio_rescue_diagnosed(btor2, &closure, reset_pinned)
    }) {
        Ok((RescueVerdict::Holds, reach)) => reach.unreachable_by,
        Ok((RescueVerdict::Violated, _)) => return (CandidateOutcome::ReachableButOpen, None),
        Ok((RescueVerdict::Inconclusive, _)) => return (CandidateOutcome::ClosureUndecided, None),
        Ok((RescueVerdict::Contradiction, _)) => return (CandidateOutcome::Contradiction, None),
        Err(why) => return (CandidateOutcome::Declined(why.to_string()), None),
    };
    // (1) reach: EF T, decided on its dual AG ¬T (a reachable `bad` is a path into T).
    let dual = match mu_parser::parse(&format!("nu Z. (!{t} && [] Z)")) {
        Ok(f) => f,
        Err(e) => return (CandidateOutcome::Declined(format!("{e:?}")), None),
    };
    let reach_engines =
        match under_check_budget(|| reach_portfolio_rescue_diagnosed(btor2, &dual, reset_pinned)) {
            Ok((RescueVerdict::Violated, reach)) => reach.reachable_by,
            Ok((RescueVerdict::Holds, _)) => {
                return (CandidateOutcome::ClosedButUnreachable, None);
            }
            Ok((RescueVerdict::Inconclusive, _)) => {
                return (CandidateOutcome::ReachUndecided, None);
            }
            Ok((RescueVerdict::Contradiction, _)) => {
                return (CandidateOutcome::Contradiction, None);
            }
            Err(why) => return (CandidateOutcome::Declined(why.to_string()), None),
        };
    let witness = under_check_budget(|| {
        crate::adapter::reach_rescue::safety_witness(btor2, &dual, reset_pinned, WITNESS_MAX_K)
    });
    (
        CandidateOutcome::Certified,
        Some(TrapCertificate {
            signal: signal.to_string(),
            value: cand.value,
            source: cand.source.clone(),
            closure_engines,
            reach_engines,
            witness,
        }),
    )
}

fn under_check_budget<T>(step: impl FnOnce() -> T) -> T {
    let check = run_budget::Budget::child(&run_budget::current(), Some(TRAP_CHECK_MS));
    let _guard = run_budget::enter(&check);
    step()
}

/// The certificate as a verdict for `AG EF good` (`good` a single `reg == value` atom): `Violated`
/// when a trap is certified from the register's value domain, `Unknown` otherwise — never `Holds`.
/// The matrix-facing entry (no report, so no sibling rows).
pub fn trap_certificate_verdict(
    btor2: &str,
    good: &str,
    reset_pinned: bool,
) -> Result<PropertyVerdict, String> {
    let good = match parse_predicate_atom_bool(good) {
        Ok(PredicateExpr::Cmp {
            register,
            op: CmpOp::Eq,
            value,
        }) => Atom {
            signal: register,
            op: CmpOp::Eq,
            value: u128::from(value),
        },
        Ok(_) => return Err(format!("`{good}` is not a single `reg == value` atom")),
        Err(e) => return Err(format!("`{good}`: {e}")),
    };
    let candidates = trap_candidates(btor2, &good, reset_pinned, &[]);
    let search = search_trap(btor2, &good, &candidates, reset_pinned);
    Ok(if search.certificate.is_some() {
        PropertyVerdict::Violated
    } else {
        PropertyVerdict::Unknown
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `st` cycles 0 → 1 → 2 → 0; the input `esc` sends it to 3, which is absorbing.
    /// `AG EF (st == 0)` is VIOLATED and `st == 3` is the trap (reachable at depth 1).
    /// The BTOR2 twin of `TRAP_FSM_SV` (`tests/differential_oracle_e2e.rs`).
    pub(crate) const TRAP_FSM: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 input 2 esc
4 state 1 st
5 zero 1
6 init 1 4 5
7 one 1
8 constd 1 2
9 constd 1 3
10 eq 2 4 9
11 eq 2 4 8
12 add 1 4 7
13 ite 1 11 5 12
14 ite 1 10 9 13
15 ite 1 3 9 14
16 next 1 4 15
";

    /// Control A — the same FSM, but 3 exits to 0 when `esc` is low: 3 is reachable and OPEN, and
    /// `AG EF (st == 0)` holds. No candidate may certify.
    const OPEN_TRAP_FSM: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 input 2 esc
4 state 1 st
5 zero 1
6 init 1 4 5
7 one 1
8 constd 1 2
9 constd 1 3
10 eq 2 4 9
11 eq 2 4 8
12 add 1 4 7
13 ite 1 11 5 12
14 ite 1 10 5 13
15 ite 1 3 9 14
16 next 1 4 15
";

    /// Control B — no edge into 3 (the `esc` mux is gone): 3 is CLOSED but unreachable, and
    /// `AG EF (st == 0)` holds. No candidate may certify.
    const UNREACHABLE_TRAP_FSM: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 input 2 esc
4 state 1 st
5 zero 1
6 init 1 4 5
7 one 1
8 constd 1 2
9 constd 1 3
10 eq 2 4 9
11 eq 2 4 8
12 add 1 4 7
13 ite 1 11 5 12
14 ite 1 10 9 13
16 next 1 4 14
";

    fn good(v: u128) -> Atom {
        Atom {
            signal: "st".into(),
            op: CmpOp::Eq,
            value: v,
        }
    }

    #[test]
    fn a_reachable_closed_state_outside_the_target_is_certified() {
        let cands = trap_candidates(TRAP_FSM, &good(0), false, &[]);
        assert!(
            cands.iter().any(|c| c.value == 3),
            "the absorbing value is a domain candidate: {cands:?}"
        );
        assert!(cands.iter().all(|c| c.value != 0), "the target is excluded");
        let s = search_trap(TRAP_FSM, &good(0), &cands, false);
        let cert = s.certificate.expect("certified");
        assert_eq!((cert.signal.as_str(), cert.value), ("st", 3));
        assert!(!cert.closure_engines.is_empty() && !cert.reach_engines.is_empty());
        let w = cert.witness.expect("a reset → trap path");
        let last = w.states.last().expect("a state");
        assert!(
            last.iter().any(|(k, v)| k == "st" && v.as_u64() == Some(3)),
            "the witness ends in the trap: {last:?}"
        );
        assert_eq!(
            trap_certificate_verdict(TRAP_FSM, "st == 0", false),
            Ok(PropertyVerdict::Violated)
        );
    }

    #[test]
    fn an_open_set_is_never_certified() {
        let cands = trap_candidates(OPEN_TRAP_FSM, &good(0), false, &[]);
        let s = search_trap(OPEN_TRAP_FSM, &good(0), &cands, false);
        assert!(s.certificate.is_none(), "{s:?}");
        assert!(
            s.tried
                .iter()
                .any(|(c, o)| c.value == 3 && *o == CandidateOutcome::ReachableButOpen),
            "3 is reachable and exits: {:?}",
            s.tried
        );
        assert_eq!(
            trap_certificate_verdict(OPEN_TRAP_FSM, "st == 0", false),
            Ok(PropertyVerdict::Unknown)
        );
    }

    #[test]
    fn a_closed_unreachable_set_is_never_certified() {
        let cands = trap_candidates(UNREACHABLE_TRAP_FSM, &good(0), false, &[]);
        let s = search_trap(UNREACHABLE_TRAP_FSM, &good(0), &cands, false);
        assert!(s.certificate.is_none(), "{s:?}");
        assert!(
            s.tried
                .iter()
                .any(|(c, o)| c.value == 3 && *o == CandidateOutcome::ClosedButUnreachable),
            "3 is closed and never entered: {:?}",
            s.tried
        );
    }

    #[test]
    fn a_free_leaf_in_the_register_cone_withholds_the_search() {
        // `esc` is an anonymous input (an undriven net): the path into 3 runs through havoc.
        let havoc = TRAP_FSM.replace("3 input 2 esc", "3 input 2");
        let cands = trap_candidates(&havoc, &good(0), false, &[]);
        let s = search_trap(&havoc, &good(0), &cands, false);
        assert!(s.certificate.is_none() && s.tried.is_empty(), "{s:?}");
        assert_eq!(s.withheld_by, vec!["input_3".to_string()]);
        // a state without `next` (a cut point) is a free leaf too
        let cut = TRAP_FSM.replace("3 input 2 esc", "3 state 2 esc_cut");
        let p = parser::parse(&cut).expect("parses");
        assert_eq!(
            free_leaves_in_cone(&p, "st"),
            Some(vec!["esc_cut".to_string()])
        );
        // …but not an undriven net: a deliberate cut is excluded from that narrower walk
        assert_eq!(undriven_nets_in_cone(&p, "st"), Some(Vec::new()));
        let p = parser::parse(&havoc).expect("parses");
        assert_eq!(
            undriven_nets_in_cone(&p, "st"),
            Some(vec!["input_3".to_string()])
        );
        // a named primary input is the environment, not a free leaf
        let p = parser::parse(TRAP_FSM).expect("parses");
        assert_eq!(free_leaves_in_cone(&p, "st"), Some(Vec::new()));
    }

    #[test]
    fn siblings_come_first_and_other_signals_are_ignored() {
        let siblings = vec![
            (
                "other_reg_terminal".to_string(),
                Atom {
                    signal: "q".into(),
                    op: CmpOp::Eq,
                    value: 1,
                },
            ),
            ("st_error_is_terminal".to_string(), good(3)),
        ];
        let cands = trap_candidates(TRAP_FSM, &good(0), false, &siblings);
        assert_eq!(
            cands.first(),
            Some(&TrapCandidate {
                value: 3,
                source: TrapSource::Sibling("st_error_is_terminal".into()),
            })
        );
        assert_eq!(
            cands.iter().filter(|c| c.value == 3).count(),
            1,
            "deduplicated against the domain: {cands:?}"
        );
    }

    #[test]
    fn a_target_that_is_not_a_register_yields_no_candidates() {
        // `out` is combinationally driven by an input: no state cell, no domain.
        const MEALY: &str = "1 sort bitvec 1\n2 input 1 in\n3 state 1 q\n4 zero 1\n5 init 1 3 4\n\
                             6 next 1 3 2\n7 uext 1 2 0 out\n";
        let g = Atom {
            signal: "out".into(),
            op: CmpOp::Eq,
            value: 0,
        };
        assert!(trap_candidates(MEALY, &g, false, &[]).is_empty());
    }
}
