//! P2 — recoverability `AG EF good` ("from every reachable state, can the design
//! still get back to a good state?"), the branching property SVA cannot express.
//!
//! # The property
//!
//! Recoverability is the CTL formula `AG EF good` — from every reachable state
//! (`AG`) there **exists** a path back to `good` (`EF`). In the modal-μ calculus it
//! is an alternating fixpoint (a greatest fixpoint wrapping a least fixpoint):
//!
//! ```text
//!   nu Y. ((mu X. (good || <> X)) && [] Y)
//! ```
//!
//! The `<>` (some-successor) inside the `[]` (all-successors) is the branching
//! content — it quantifies existentially over futures *inside* a universal envelope,
//! which is exactly what a linear formalism (LTL / SVA) cannot state. See
//! [`docs/design/recoverability-vs-sva.md`](../../../docs/design/recoverability-vs-sva.md).
//!
//! # How it is decided
//!
//! This module offers the ergonomic entry point: name the `good` atom, and it builds
//! the `AG EF good` formula and decides it with the **exact 3-valued symbolic
//! engine** ([`crate::adapter::btor2::symbolic_bitblast::exact_symbolic_verdict`]) —
//! sound at every alternation depth (Bruns–Godefroid), definite within the engine's
//! 40-bit cone cap. Over the cap it abstains (`Unknown`).
//!
//! For designs wider than the exact cap, the **predicate-cube + `smt-hyper-must`**
//! path (`mununu btor2 cegar --formula … --must-edge-inference smt-hyper-must`)
//! decides the same formula via abstraction — the path the V.7-c OpenTitan `csrng`
//! recoverability showcase uses.
//!
//! P2 Slice 1 wires that path in **automatically**: [`verify_recoverability`] tries
//! the exact engine first and, when it abstains (over the ~40-bit cone cap or an
//! unsupported construct), escalates to [`verify_recoverability_scalable`] — the
//! predicate-cube `smt-hyper-must` reduction — so the νμ property decides beyond 40
//! bits. The escalation mirrors the verify-auto safety-⊥ `reach_rescue` escalation.
//! Every scalable verdict is cross-checked against the exact engine on the small
//! fixtures (the differential-oracle soundness gate; see the module tests).

use std::collections::BTreeMap;

use crate::adapter::AdapterOptions;
use crate::adapter::btor2::cegar::{
    CegarOptions, LiftStrategy, PredicateSource, cegar_refine_loop, config_values_to_sidecar_json,
};
use crate::adapter::btor2::kmts_lift::{MayEdgeInference, MustEdgeInference, PredicateSpec};
use crate::adapter::btor2::predicate_expr::{CmpOp, PredicateExpr, parse_predicate_expr};
use crate::adapter::btor2::symbolic_bitblast::exact_symbolic_verdict;
use crate::mu_calculus::Environment;
use crate::mu_calculus::parser as mu_parser;
use crate::mu_calculus::trit::Trit;
use crate::verdict::{
    AssumptionKind, ConfigPartition, DiscoveredAssumption, PropertyVerdict, VacuityWitness,
    VerdictRefinement,
};

/// Max CEGAR refinement iterations for the scalable recoverability path — a sensible
/// default: a pure state-atom `AG EF` target either decides at the seed abstraction or
/// after a few weakest-precondition splits.
const RECOVERABILITY_MAX_ITERATIONS: usize = 8;

/// Extract `state_register == constant` guard atoms from the design's `Eq` comparison nodes — the
/// decision conditions the control logic branches on. Seeds the datapath predicates a
/// datapath-DEPENDENT recoverability return needs (Class 2): `busy → done` gated on `data == K`
/// yields the atom `(data, K)`, where K is a design literal (not enumerable over `2^W`). Returns only
/// comparisons of a STATE register to a resolvable CONSTANT; deduped.
fn eq_guard_atoms(file: &crate::adapter::btor2::ast::Btor2File) -> Vec<(String, u64)> {
    use crate::adapter::btor2::ast::{Node, Op};
    let symbols = crate::adapter::btor2::parser::collect_symbols(file);
    let state_nids: std::collections::HashSet<i64> = file
        .lines
        .iter()
        .filter(|l| matches!(l.node, Node::State { .. }))
        .map(|l| l.nid)
        .collect();
    let mut out: Vec<(String, u64)> = Vec::new();
    for line in &file.lines {
        let Node::Op {
            op: Op::Eq, args, ..
        } = &line.node
        else {
            continue;
        };
        if args.len() != 2 {
            continue;
        }
        // Either operand may be the register vs the constant.
        for (reg_nid, const_nid) in [
            (args[0].0.abs(), args[1].0.abs()),
            (args[1].0.abs(), args[0].0.abs()),
        ] {
            if state_nids.contains(&reg_nid)
                && let Some(sym) = symbols.get(&reg_nid)
                && let Some(k) =
                    crate::adapter::btor2::bit_blast::resolve_btor2_constant(file, const_nid)
                && !out.iter().any(|(s, v)| s == sym && *v == k)
            {
                out.push((sym.clone(), k));
            }
        }
    }
    out
}

/// N1 boolean-gated-event rewrite — given a `good` EVENT signal `sig` (a 1-bit output like a
/// watchdog `timeout` or a `done` strobe) that is driven **EXACTLY** by a `counter == constant`
/// comparison (`done <= (cnt == 0)`; registered OR combinational), return `(counter_register,
/// threshold)`. The recoverability of the event — `AG EF (sig == 1)` — then reduces to the counter
/// recoverability `AG EF (counter == threshold)`, which the RANKING certificate decides (a boolean
/// `sig` carries no descending measure; its driving counter does). **SOUND only for a PURE `Eq`
/// driver:** if `sig`'s next/definition is an `Ite`/`And`/anything other than a bare `Eq(state,
/// const)` (i.e. it also depends on a reset gate, an enable, another condition), the equivalence
/// `sig == 1 ⟺ counter == threshold` breaks (`sig` could be 0 while `counter == threshold`), so any
/// non-`Eq` driver returns `None` (abstain — never a rewrite that could fabricate a Holds). The
/// one-cycle register delay is absorbed by `EF` (reaching the counter value one step before `sig`).
fn counter_gate_of(
    file: &crate::adapter::btor2::ast::Btor2File,
    sig: &str,
) -> Option<(String, u64)> {
    use crate::adapter::btor2::ast::{Node, Op};
    let symbols = crate::adapter::btor2::parser::collect_symbols(file);
    let sig_nid = *symbols
        .iter()
        .find(|(_, s)| s.as_str() == sig)
        .map(|(n, _)| n)?;
    // The node that DEFINES `sig`: a state register's `next` value, or the op line itself
    // (a combinational output carrying the symbol).
    let driver_nid = match &file.lookup(sig_nid)?.node {
        Node::State { .. } => {
            crate::adapter::btor2::parser::find_next_value_operand(file, sig_nid)?
                .0
                .abs()
        }
        _ => sig_nid,
    };
    // The driver must be a BARE `Eq` — no surrounding mux/gate (soundness of the rewrite).
    let Node::Op {
        op: Op::Eq, args, ..
    } = &file.lookup(driver_nid)?.node
    else {
        return None;
    };
    if args.len() != 2 {
        return None;
    }
    let state_nids: std::collections::HashSet<i64> = file
        .lines
        .iter()
        .filter(|l| matches!(l.node, Node::State { .. }))
        .map(|l| l.nid)
        .collect();
    // One operand is the counter STATE register, the other a resolvable CONSTANT threshold.
    for (reg_nid, const_nid) in [
        (args[0].0.abs(), args[1].0.abs()),
        (args[1].0.abs(), args[0].0.abs()),
    ] {
        if state_nids.contains(&reg_nid)
            && let Some(counter) = symbols.get(&reg_nid)
            && let Some(k) =
                crate::adapter::btor2::bit_blast::resolve_btor2_constant(file, const_nid)
        {
            return Some((counter.clone(), k));
        }
    }
    None
}

/// N1 Class-2 RELATIONAL discovery — the datapath-dependent return whose guard compares two STATE
/// registers (`busy → done` gated on `data == target`), not a register to a constant. Extracts the
/// `(lhs, rhs)` register pairs from the design's `Eq` comparison nodes where BOTH operands are state
/// registers — the relational decision conditions the control logic branches on. The seeded relational
/// predicate lets the recoverability return decide at SCALE: the exact hyper-must edge preserves the
/// invariant (`data == target ⟹ data' == target'`) across the wide datapath WITHOUT concretising the
/// register values (which are not enumerable over `2^W`); the may side may UF-wrap the increments, but
/// that only pushes toward `⊥` (sound, per PR #302's universal ◇). Pairs are ordered (`lhs <= rhs`) for
/// deterministic dedup (`data == target` and `target == data` are the same predicate); reflexive
/// `r == r` is dropped (no information). Deduped.
/// SEL (P1-a, shot ①b) — discover **array-content** guard atoms `array[index] == K`:
/// an `Eq` comparing a memory `Read(array, index)` against a constant `K` — the
/// design's own array-content decision condition (e.g. `busy → idle` gated on
/// `mem[key] == unlock`). Returns `(array_symbol, index_symbol, value)`; the caller
/// lifts each to a `PredicateExpr::Select` COMPOUND seed. Mirrors [`eq_guard_atoms`]
/// (register-vs-constant) for the array-read case — the property-directed discovery
/// of the array-content predicate the predicate-cube KMTS needs to decide a
/// content-gated recovery (the ROBDD exact engine cannot, since arrays are not
/// bit-blastable). Bounded by the number of `Eq(Read, const)` nodes; deduped.
fn select_guard_atoms(file: &crate::adapter::btor2::ast::Btor2File) -> Vec<(String, String, u64)> {
    use crate::adapter::btor2::ast::{Node, Op};
    let symbols = crate::adapter::btor2::parser::collect_symbols(file);
    // nid → (array_nid, index_nid) for every memory `Read` op.
    let reads: std::collections::HashMap<i64, (i64, i64)> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Op {
                op: Op::Read, args, ..
            } if args.len() == 2 => Some((l.nid, (args[0].0.abs(), args[1].0.abs()))),
            _ => None,
        })
        .collect();
    let state_nids: std::collections::HashSet<i64> = file
        .lines
        .iter()
        .filter(|l| matches!(l.node, Node::State { .. }))
        .map(|l| l.nid)
        .collect();
    // Resolve a read-index nid to a symboled register. yosys routinely reset-muxes the
    // addressing FF (`read(mem, ite(rst, key, 0))`) and — crucially — `collect_symbols`
    // attaches the visible name (`u.key`) to the underlying STATE cell, NOT to the `ite`/
    // `uext` chain. So a bare `symbols.get(idx_nid)` on the reset-mux node misses it; walk
    // the index cone to the first symboled STATE instead (collecting only states skips the
    // reset-CONDITION input the `ite` branches on). That symboled state name is a real
    // `Node::State` symbol, so it resolves in the states-only cube `nid_map` AND is pinned
    // in `init_values` (the reset-cube gate) — the seeded `select(arr, key)` then reads
    // exactly the cell the design reads, over the same state frame as the BV atoms.
    let idx_symbol = |idx_nid: i64| -> Option<String> {
        if let Some(s) = symbols.get(&idx_nid) {
            return Some(s.clone());
        }
        let mut seen = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::from([idx_nid]);
        while let Some(nid) = queue.pop_front() {
            if !seen.insert(nid) {
                continue;
            }
            let Some(line) = file.lookup(nid) else {
                continue;
            };
            match &line.node {
                Node::State { .. } => {
                    if let Some(s) = symbols.get(&nid) {
                        return Some(s.clone());
                    }
                }
                Node::Op { args, .. } => {
                    for o in args {
                        queue.push_back(o.0.abs());
                    }
                }
                _ => {}
            }
        }
        None
    };
    let mut out: Vec<(String, String, u64)> = Vec::new();
    for line in &file.lines {
        let Node::Op {
            op: Op::Eq, args, ..
        } = &line.node
        else {
            continue;
        };
        if args.len() != 2 {
            continue;
        }
        // Either operand may be the array `Read` vs the constant.
        for (read_nid, const_nid) in [
            (args[0].0.abs(), args[1].0.abs()),
            (args[1].0.abs(), args[0].0.abs()),
        ] {
            let Some(&(arr_nid, idx_nid)) = reads.get(&read_nid) else {
                continue;
            };
            // The array must be an array-sorted state cell (a `$mem`) with a symbol; the
            // index resolves to a symboled alias (see-through above) so the Select binds.
            if !state_nids.contains(&arr_nid) {
                continue;
            }
            let Some(arr_sym) = symbols.get(&arr_nid) else {
                continue;
            };
            let Some(idx_sym) = idx_symbol(idx_nid) else {
                continue;
            };
            let Some(k) = crate::adapter::btor2::bit_blast::resolve_btor2_constant(file, const_nid)
            else {
                continue;
            };
            if !out
                .iter()
                .any(|(a, i, v)| a == arr_sym && *i == idx_sym && *v == k)
            {
                out.push((arr_sym.clone(), idx_sym, k));
            }
        }
    }
    out
}

fn eq_reg_guard_atoms(file: &crate::adapter::btor2::ast::Btor2File) -> Vec<(String, String)> {
    use crate::adapter::btor2::ast::{Node, Op};
    let symbols = crate::adapter::btor2::parser::collect_symbols(file);
    let state_nids: std::collections::HashSet<i64> = file
        .lines
        .iter()
        .filter(|l| matches!(l.node, Node::State { .. }))
        .map(|l| l.nid)
        .collect();
    let mut out: Vec<(String, String)> = Vec::new();
    for line in &file.lines {
        let Node::Op {
            op: Op::Eq, args, ..
        } = &line.node
        else {
            continue;
        };
        if args.len() != 2 {
            continue;
        }
        let (a, b) = (args[0].0.abs(), args[1].0.abs());
        if a == b {
            continue; // reflexive `r == r` carries no information
        }
        if state_nids.contains(&a)
            && state_nids.contains(&b)
            && let (Some(sa), Some(sb)) = (symbols.get(&a), symbols.get(&b))
        {
            let (lhs, rhs) = if sa <= sb {
                (sa.clone(), sb.clone())
            } else {
                (sb.clone(), sa.clone())
            };
            if !out.iter().any(|(l, r)| l == &lhs && r == &rhs) {
                out.push((lhs, rhs));
            }
        }
    }
    out
}

/// N1 frontier (b′) — arithmetic-relational discovery: the datapath-dependent return whose guard is
/// `data == target + K` (a register compared to ANOTHER register plus a constant addend), not a bare
/// `register == register`. The value that unblocks the return (`target + K`) is neither a design literal
/// nor a syntactic single-register comparison, so the literal- and relational-guard extractions both
/// miss it — but the design DOES contain it as `eq(reg, add(reg, const))`. Extracting that pattern yields
/// the `PredicateExpr::CmpRegAddend` the return needs; like the relational case, the exact hyper-must
/// edge preserves the invariant (`data == target + K ⟹ data' == target' + K`, in mod-2^width BV) across
/// the wide increments without concretising the registers. Returns `(lhs_reg, rhs_reg, addend, width)`;
/// `width` is the register width (for the mod-2^width arithmetic the reset-cube eval needs). Skips the
/// degenerate `reg == reg + K` (same register). Deduped.
fn eq_reg_addend_guard_atoms(
    file: &crate::adapter::btor2::ast::Btor2File,
) -> Vec<(String, String, u64, u32)> {
    use crate::adapter::btor2::ast::{Node, Op};
    let symbols = crate::adapter::btor2::parser::collect_symbols(file);
    let state_nids: std::collections::HashSet<i64> = file
        .lines
        .iter()
        .filter(|l| matches!(l.node, Node::State { .. }))
        .map(|l| l.nid)
        .collect();
    let state_width = |nid: i64| -> Option<u32> {
        match file.lookup(nid).map(|l| &l.node) {
            Some(Node::State { sort, .. }) => crate::adapter::btor2::parser::bv_width(file, *sort),
            _ => None,
        }
    };
    let mut out: Vec<(String, String, u64, u32)> = Vec::new();
    for line in &file.lines {
        let Node::Op {
            op: Op::Eq, args, ..
        } = &line.node
        else {
            continue;
        };
        if args.len() != 2 {
            continue;
        }
        // One operand is a state register `lhs`; the other is an `add(state, const)` node.
        for (reg_nid, other_nid) in [
            (args[0].0.abs(), args[1].0.abs()),
            (args[1].0.abs(), args[0].0.abs()),
        ] {
            if !state_nids.contains(&reg_nid) {
                continue;
            }
            let Some(lhs_sym) = symbols.get(&reg_nid) else {
                continue;
            };
            let Some(Node::Op {
                op: Op::Add,
                args: aa,
                ..
            }) = file.lookup(other_nid).map(|l| &l.node)
            else {
                continue;
            };
            if aa.len() != 2 {
                continue;
            }
            // The add is `rhs_state + const` (either operand order).
            for (rs_nid, ac_nid) in [
                (aa[0].0.abs(), aa[1].0.abs()),
                (aa[1].0.abs(), aa[0].0.abs()),
            ] {
                if state_nids.contains(&rs_nid)
                    && let Some(rhs_sym) = symbols.get(&rs_nid)
                    && rhs_sym != lhs_sym
                    && let Some(addend) =
                        crate::adapter::btor2::bit_blast::resolve_btor2_constant(file, ac_nid)
                    && let Some(width) = state_width(reg_nid)
                    && width > 0
                    && !out
                        .iter()
                        .any(|(l, r, a, _)| l == lhs_sym && r == rhs_sym && *a == addend)
                {
                    out.push((lhs_sym.clone(), rhs_sym.clone(), addend, width));
                }
            }
        }
    }
    out
}

/// N1 EMERGENT-K — discover inductive RELATIONAL invariants (`a == b`, `a == b + k`) that hold on every
/// reachable state but have NO syntactic comparison node in the design, so the guard-atom extractors
/// ([`eq_reg_guard_atoms`] / [`eq_reg_addend_guard_atoms`], which read `eq`/`add` nodes) miss them
/// entirely. This is the emergent case: a design that decides its control return through *inequalities*
/// (`a >= b && b >= a`) or arithmetic carries the relation `a == b` only *semantically*, never as a form
/// the syntactic seeders can lift.
///
/// **Mechanism (sound inductive-invariant discovery).** For each same-width pair of state registers, one
/// SMT query over the EXACT transition asks whether the difference `a - b` is preserved by *every*
/// transition — `T ∧ (a' - b' != a - b)` UNSAT. If so, the reachable states all satisfy
/// `a - b == (a_init - b_init)` (base: the initial state fixes the difference; step: every transition
/// preserves it), a 1-inductive relational invariant. It is seeded as a compound predicate (`a == b` when
/// the reset difference is 0, else `a == b + k`), so the cube can refute the *spurious* havoc-equal (or
/// havoc-unequal) states the wide-datapath UF-wrap admits — exactly the states that leave the cube at ⊥.
///
/// **SOUNDNESS.** An inductive invariant over-approximates the reachable set, so adding it as a cube
/// dimension only refines the abstraction monotonically (Shoham–Grumberg) — it can never flip a definite
/// verdict, and the discovery need not be complete. The preservation check quantifies over ALL inputs
/// (the transition relation carries the free inputs), so a relation some input could break is simply not
/// discovered: fewer seeds, never a wrong one. The direction with the smaller addend is chosen for a
/// tidy `a == b + k`; either direction denotes the same relation.
///
/// Returns `(name, expr_str, expr)` tuples ready to append to `compound_seeds`, deduplicated by the
/// unordered register pair, COI-filtered to the good register's cone, and capped by `budget`.
fn discover_inductive_relational_invariants(
    file: &crate::adapter::btor2::ast::Btor2File,
    init_values: &std::collections::BTreeMap<String, u128>,
    good_coi: &std::collections::HashSet<String>,
    budget: usize,
) -> Vec<(String, String, PredicateExpr)> {
    if budget == 0 {
        return Vec::new();
    }
    // Bound the O(n^2) pair search: real designs have few state registers in a good register's cone, and
    // each pair costs one SMT solve. A hard cap keeps a pathological register count from stalling.
    const MAX_DISCOVERY_QUERIES: usize = 128;
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let Ok(view) = crate::adapter::btor2::kmts_lift::encode_design_for_lift(file) else {
            return Vec::new();
        };
        let nid_map = crate::adapter::btor2::smt_must_edge::build_register_nid_map(&view);
        let in_coi = |r: &str| good_coi.is_empty() || good_coi.contains(r);
        // Candidate registers: state cells that carry a reset value and lie in the good cone. Sorted by
        // name for a deterministic search order (and so the smaller-addend tie-break is stable).
        let mut regs: Vec<(String, i64, u32)> = nid_map
            .iter()
            .filter_map(|(name, &nid)| {
                if !in_coi(name) || !init_values.contains_key(name) {
                    return None;
                }
                let w = view.curr_state(nid)?.get_size();
                Some((name.clone(), nid, w))
            })
            .collect();
        regs.sort_unstable();

        let mut params = z3::Params::new();
        params.set_u32("timeout", 5000);
        let mut out: Vec<(String, String, PredicateExpr)> = Vec::new();
        let mut seen_pairs: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        let mut queries = 0usize;
        'outer: for i in 0..regs.len() {
            for j in (i + 1)..regs.len() {
                if out.len() >= budget || queries >= MAX_DISCOVERY_QUERIES {
                    break 'outer;
                }
                let (a_name, a_nid, aw) = &regs[i];
                let (b_name, b_nid, bw) = &regs[j];
                if aw != bw {
                    continue;
                }
                let (Some(&a_init), Some(&b_init)) =
                    (init_values.get(a_name), init_values.get(b_name))
                else {
                    continue;
                };
                let (Some(ac), Some(an), Some(bc), Some(bn)) = (
                    view.curr_state(*a_nid),
                    view.next_state(*a_nid),
                    view.curr_state(*b_nid),
                    view.next_state(*b_nid),
                ) else {
                    continue;
                };
                // Difference-preservation: is `a - b` constant across every transition?
                queries += 1;
                let solver = z3::Solver::new();
                solver.set_params(&params);
                solver.assert(&view.transition);
                let diff_curr = ac.bvsub(bc);
                let changed = an.bvsub(bn).eq(&diff_curr).not();
                solver.assert(&changed);
                if !matches!(solver.check(), z3::SatResult::Unsat) {
                    // Difference varies — try an ORDERING inequality (`a <= b`) that a varying gap may
                    // still preserve (FIFO pointers, a catch-up counter), in the direction the reset gap
                    // fixes: `a_init <= b_init` ⇒ candidate `a <= b`. One more SMT query per pair. SOUND:
                    // init establishes `lo_init <= hi_init`; `T ∧ (lo<=hi) ∧ (lo'>hi')` UNSAT is the
                    // inductive step, so `lo <= hi` holds on every reachable state.
                    if out.len() < budget && queries < MAX_DISCOVERY_QUERIES {
                        queries += 1;
                        let (lo_c, lo_n, hi_c, hi_n, lname, hname) = if a_init <= b_init {
                            (ac, an, bc, bn, a_name, b_name)
                        } else {
                            (bc, bn, ac, an, b_name, a_name)
                        };
                        let s2 = z3::Solver::new();
                        s2.set_params(&params);
                        s2.assert(&view.transition);
                        let ord = lo_c.bvule(hi_c);
                        let broken = lo_n.bvugt(hi_n);
                        s2.assert(&ord);
                        s2.assert(&broken);
                        if matches!(s2.check(), z3::SatResult::Unsat) {
                            seen_pairs.insert((a_name.clone(), b_name.clone()));
                            let expr_str = format!("{lname} <= {hname}");
                            if let Ok(expr) = parse_predicate_expr(&expr_str) {
                                out.push((format!("ind_{lname}_le_{hname}"), expr_str, expr));
                            }
                        }
                    }
                    continue; // difference not invariant; inequality (if any) already seeded above
                }
                if !seen_pairs.insert((a_name.clone(), b_name.clone())) {
                    continue;
                }
                // The difference is the constant `a_init - b_init`. Seed the relation.
                if a_init == b_init {
                    let expr_str = format!("{a_name} == {b_name}");
                    let Ok(expr) = parse_predicate_expr(&expr_str) else {
                        continue;
                    };
                    out.push((format!("ind_{a_name}_eq_{b_name}"), expr_str, expr));
                    continue;
                }
                let w = *aw;
                if w > 64 {
                    continue; // an addend wider than u64 — a == b (above) still covers the equal case
                }
                let modulus_m1: u128 = if w >= 64 {
                    u64::MAX as u128
                } else {
                    (1u128 << w) - 1
                };
                let modulus: u128 = modulus_m1 + 1;
                let diff = a_init.wrapping_sub(b_init) & modulus_m1; // (a_init - b_init) mod 2^w, in [1, 2^w-1]
                // Choose the direction with the smaller addend: `a == b + diff` vs `b == a + (2^w-diff)`.
                let (lhs, rhs, addend) = if diff <= modulus / 2 {
                    (a_name.clone(), b_name.clone(), diff as u64)
                } else {
                    (b_name.clone(), a_name.clone(), (modulus - diff) as u64)
                };
                let expr_str = format!("{lhs} == {rhs} + {addend}");
                out.push((
                    format!("ind_{lhs}_eq_{rhs}_plus_{addend}"),
                    expr_str,
                    PredicateExpr::CmpRegAddend {
                        lhs,
                        op: CmpOp::Eq,
                        rhs,
                        addend,
                        width: w,
                    },
                ));
            }
        }
        out
    })
}

/// IC3ia I1 (2026-07-12) — HOUDINI relative-inductive filter. Given candidate predicates, return the
/// largest subset whose CONJUNCTION is an inductive invariant of the design, over the EXACT transition:
/// every survivor holds at the initial state AND is preserved *assuming the whole surviving conjunction*
/// (not individually). This is the step the one-shot discovery ([`discover_inductive_relational_invariants`],
/// which only keeps *individually* 1-inductive relations) structurally cannot take: it finds conjunctive
/// invariants `P1 ∧ P2` where **neither `Pi` is inductive alone** but the pair is — the relative induction
/// the diagnosis identified as the real gap. The de-risk (I1) of the IC3ia-on-the-cube direction.
///
/// **Algorithm (Houdini greatest-fixpoint).** Start with every candidate that holds at init; repeatedly
/// drop any `P` for which `(⋀ set) ∧ T ∧ ¬P'` is SAT (i.e. `P` is not implied one step from the current
/// conjunction), until no candidate is dropped. The surviving conjunction is inductive.
///
/// **SOUNDNESS.** The result holds at init and is closed under `T` (over the exact transition), so it
/// over-approximates the reachable set — seeding it (each survivor a cube dimension) only refines the
/// abstraction monotonically (Shoham–Grumberg), never flipping a definite verdict.
fn houdini_inductive_conjunction(
    file: &crate::adapter::btor2::ast::Btor2File,
    init_values: &std::collections::BTreeMap<String, u128>,
    candidates: Vec<PredicateExpr>,
) -> Vec<PredicateExpr> {
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let Ok(view) = crate::adapter::btor2::kmts_lift::encode_design_for_lift(file) else {
            return Vec::new();
        };
        let nid_map = crate::adapter::btor2::smt_must_edge::build_register_nid_map(&view);
        let curr = |name: &str| nid_map.get(name).and_then(|n| view.curr_state(*n)).cloned();
        let next = |name: &str| nid_map.get(name).and_then(|n| view.next_state(*n)).cloned();
        let init_map: std::collections::HashMap<String, u128> =
            init_values.iter().map(|(k, v)| (k.clone(), *v)).collect();

        // Keep candidates that hold at INIT and whose every register resolves to a state cell (so both
        // current and next constraints build). `(expr, curr_bool, next_bool)`.
        let mut set: Vec<(PredicateExpr, z3::ast::Bool, z3::ast::Bool)> = Vec::new();
        for c in candidates {
            if !c.eval(&init_map) {
                continue;
            }
            let (Some(cc), Some(cn)) = (c.build_constraint(&curr), c.build_constraint(&next))
            else {
                continue;
            };
            set.push((c, cc, cn));
        }

        let mut params = z3::Params::new();
        params.set_u32("timeout", 3000);
        // Bound the total SMT budget: a wide `bvmul` in the exact transition makes each query expensive,
        // and the fixpoint is `O(candidates²)`. Cutting off early is SOUND — the survivors are only cube
        // DIMENSIONS (evaluated exactly per-cube), never asserted as invariants, so a not-fully-filtered
        // set costs refinement power, never soundness. A per-query timeout also drops a candidate (treated
        // as not-inductive), which is likewise sound.
        const MAX_HOUDINI_QUERIES: usize = 48;
        let mut queries = 0usize;
        // Houdini fixpoint: drop any P not inductive relative to the (shrinking) conjunction.
        'fixpoint: loop {
            let conj: Vec<&z3::ast::Bool> = set.iter().map(|(_, cc, _)| cc).collect();
            let mut keep = vec![true; set.len()];
            let mut removed = false;
            for (i, (_, _, cn)) in set.iter().enumerate() {
                if queries >= MAX_HOUDINI_QUERIES {
                    break 'fixpoint;
                }
                queries += 1;
                let solver = z3::Solver::new();
                solver.set_params(&params);
                for &c in &conj {
                    solver.assert(c);
                }
                solver.assert(&view.transition);
                let neg = cn.not();
                solver.assert(&neg);
                // SAT ⇒ a transition from the conjunction reaches ¬P' ⇒ P not relatively inductive.
                if !matches!(solver.check(), z3::SatResult::Unsat) {
                    keep[i] = false;
                    removed = true;
                }
            }
            if !removed {
                break;
            }
            let mut idx = 0usize;
            set.retain(|_| {
                let k = keep[idx];
                idx += 1;
                k
            });
        }
        set.into_iter().map(|(e, _, _)| e).collect()
    })
}

/// N1 frontier (a) — the good register's cone-of-influence: the state registers whose values determine
/// when `good` is (re)reached — those reachable backward from the good register's next-state function,
/// plus the register itself. A guard atom over a register OUTSIDE this cone (a decoy comparison on an
/// unrelated register) cannot change the recoverability verdict, yet seeding it still doubles the cube
/// (`2^{|P|}`) — the un-directed eager extraction's scaling wall (a design with many unrelated guards
/// blows up). Restricting the guard-atom seeds to this cone is the property-directed discovery of
/// frontier (a): only the guards the return path actually branches on are seeded, at cone-of-influence
/// granularity (a sound over-approximation of the strictly per-⊥-classifying-transition guard set).
/// Returns the register NAMES in the cone; an EMPTY set (good register absent, or no restriction found)
/// means "do not restrict" so the caller keeps its prior, unfiltered behaviour.
fn good_register_coi(
    file: &crate::adapter::btor2::ast::Btor2File,
    good_register: &str,
) -> std::collections::HashSet<String> {
    use crate::adapter::btor2::parser::{
        collect_reachable_states_from, collect_symbols, find_next_value_operand,
    };
    let symbols = collect_symbols(file);
    let Some(seed_nid) = symbols
        .iter()
        .find_map(|(nid, name)| (name == good_register).then_some(*nid))
    else {
        return std::collections::HashSet::new(); // good register absent → no restriction
    };
    let mut reachable: std::collections::HashSet<i64> = std::collections::HashSet::new();
    reachable.insert(seed_nid);
    if let Some(next_value) = find_next_value_operand(file, seed_nid) {
        reachable.extend(collect_reachable_states_from(
            file,
            std::slice::from_ref(&next_value),
        ));
    }
    reachable
        .iter()
        .filter_map(|nid| symbols.get(nid).cloned())
        .collect()
}

/// N1 frontier (a) — the constant literals appearing in the good register's next-state cone (the values
/// its control logic assigns or branches on). Directs the control-state candidate pool: enumerating
/// `good == v` for EVERY global constant (the prior behaviour) seeds control predicates the good
/// register can never take — decoy constants that belong to unrelated registers — and each is a cube
/// dimension, so a design with many unrelated constants blows up the `2^{|P|}` all-pairs SMT even though
/// none of those predicates can change the verdict. Restricting to the good register's own cone keeps
/// the control-state seeding property-directed. An EMPTY result (good register absent / no `next`) means
/// "no cone info" so the caller falls back to `{0,1} ∪ global constants`.
/// The bit-width of the `state` cell `name` (by its symbol-table name), or `None` if absent.
fn register_width(file: &crate::adapter::btor2::ast::Btor2File, name: &str) -> Option<u32> {
    use crate::adapter::btor2::ast::Node;
    let symbols = crate::adapter::btor2::parser::collect_symbols(file);
    file.lines.iter().find_map(|l| match &l.node {
        Node::State { sort, .. } if symbols.get(&l.nid).is_some_and(|s| s == name) => {
            crate::adapter::btor2::parser::bv_width(file, *sort)
        }
        _ => None,
    })
}

fn good_next_cone_constants(
    file: &crate::adapter::btor2::ast::Btor2File,
    good_register: &str,
) -> Vec<u64> {
    use crate::adapter::btor2::ast::Node;
    use crate::adapter::btor2::parser::{collect_symbols, find_next_value_operand};
    let symbols = collect_symbols(file);
    let Some(seed_nid) = symbols
        .iter()
        .find_map(|(nid, name)| (name == good_register).then_some(*nid))
    else {
        return Vec::new();
    };
    let Some(next_value) = find_next_value_operand(file, seed_nid) else {
        return Vec::new();
    };
    let mut queue: std::collections::VecDeque<i64> =
        std::collections::VecDeque::from([next_value.0.abs()]);
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut consts: Vec<u64> = Vec::new();
    while let Some(nid) = queue.pop_front() {
        if !seen.insert(nid) {
            continue;
        }
        if let Some(v) = crate::adapter::btor2::bit_blast::resolve_btor2_constant(file, nid)
            && !consts.contains(&v)
        {
            consts.push(v);
        }
        if let Some(line) = file.lookup(nid)
            && let Node::Op { args, .. } = &line.node
        {
            for o in args {
                queue.push_back(o.0.abs());
            }
        }
    }
    consts
}

/// N1 ranking helper — the INPUT nids in the next-state cone of the given (good) registers: the inputs
/// that can affect the RANKING's evolution. The ∃-input enumeration pins only these; a real design
/// often carries wide inputs outside this cone (an FPV backdoor, a hardened counter's SECONDARY-register
/// or dead-output logic — e.g. OpenTitan `prim_count`) that would otherwise swamp the enumeration's bit
/// cap. Leaving an out-of-cone input FREE rather than pinning it is SOUND — pinning fewer inputs makes
/// the UNSAT check STRONGER (∀ over the free ones), so a certified verdict stays a real witness.
fn inputs_in_next_cone(
    file: &crate::adapter::btor2::ast::Btor2File,
    seed_state_nids: &[i64],
) -> std::collections::HashSet<i64> {
    use crate::adapter::btor2::ast::Node;
    let mut queue: std::collections::VecDeque<i64> = std::collections::VecDeque::new();
    for &s in seed_state_nids {
        if let Some(nv) = crate::adapter::btor2::parser::find_next_value_operand(file, s) {
            queue.push_back(nv.0.abs());
        }
    }
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut inputs: std::collections::HashSet<i64> = std::collections::HashSet::new();
    while let Some(nid) = queue.pop_front() {
        if !seen.insert(nid) {
            continue;
        }
        match file.lookup(nid).map(|l| &l.node) {
            Some(Node::Input { .. }) => {
                inputs.insert(nid);
            }
            Some(Node::Op { args, .. }) => {
                for o in args {
                    queue.push_back(o.0.abs());
                }
            }
            _ => {}
        }
    }
    inputs
}

/// N1 ranking certificate for `AG EF good` — the well-founded-descent / RANKING class the predicate
/// cube cannot capture. When `good` is reached by a monotone datapath descent (a down-counter to 0, a
/// timer, a drain), no bounded predicate set represents the descent, so the cube abstains (`Unknown`) —
/// yet the property HOLDS. This proves it directly over the EXACT transition (Podelski–Rybalchenko),
/// sidestepping the cube. For a candidate ranking δ — `good = (r == V)` → δ = `r - V` (a DESCENT of r to
/// V) or δ = `V - r` (an ASCENT of r to V); `good = (a == b)` → δ = `a - b` or `b - a` (unsigned BV
/// difference; both directions are tried) — it checks:
///   (2) **`transition ∧ ¬good ∧ δ_next ≥ δ_curr` is UNSAT** — i.e. EVERY transition out of a non-good
///       state STRICTLY decreases δ; combined with (1) δ ≥ 0 (trivial for an unsigned BV), δ cannot
///       decrease forever, so every path reaches good.
/// Two attempts, in order of cost:
///   **(A) all-path** — `transition ∧ ¬good ∧ (δ_next ≥ δ_curr)` UNSAT: EVERY input choice out of a
///   non-good state strictly decreases δ. No quantifier (inputs free), fast; proves the stronger
///   `AG AF good` ⊆ `AG EF good`. Decides deterministic descents (down-counters, timers).
///   **(B) some-path** — decides a relation drained by only SOME input (a FIFO's read-gated
///   `wptr == rptr`, `AG EF` but not `AG AF`, since the environment may write forever) which (A) cannot.
///   Rather than a flaky quantified-BV query, it ENUMERATES constant input valuations: if some fixed
///   input `v` makes `transition ∧ ¬good ∧ (inputs == v) ∧ (δ_next ≥ δ_curr)` UNSAT, the constant-`v`
///   path strictly descends δ from every non-good state, so it reaches good ⇒ `AG EF good`. Capped at a
///   few input bits (a drain is gated on narrow control inputs), a non-quantified fast path.
/// Each attempt runs first with the single-register measure δ1, then with LEXICOGRAPHIC measures
/// `(δ1, s)` — δ1 paired with each other register `s` as a tiebreaker — so a NESTED descent decides:
/// when δ1 alone does not strictly decrease every step (it holds while an inner counter runs down), the
/// tuple `(δ1, s)` does, and lex order on the tuple is well-founded (`non_dec` becomes
/// `¬[(δ1' < δ1) ∨ (δ1' == δ1 ∧ s' < s)]`).
/// With δ ≥ 0 (trivial for an unsigned BV) either certificate ⇒ `EF good` from every state ⇒ `AG EF good`,
/// so returning `Holds` is SOUND. Both correctly FAIL on wrap/overshoot (`cnt - 2` from an odd start
/// never hits 0 → the difference jumps up → SAT) and on non-monotone designs (a sound fall-through to the
/// cube — the certificate is sufficient, not necessary).
fn ranking_certificate_holds(
    file: &crate::adapter::btor2::ast::Btor2File,
    good_registers: &[String],
    good_value: Option<u64>,
) -> bool {
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let Ok(view) = crate::adapter::btor2::kmts_lift::encode_design_for_lift(file) else {
            return false;
        };
        let nid_map = crate::adapter::btor2::smt_must_edge::build_register_nid_map(&view);
        let mask = |w: u32| if w >= 64 { u64::MAX } else { (1u64 << w) - 1 };
        // PRIMARY ranking components + the ¬good constraint + the good register NIDs. BOTH directions are
        // tried: a DESCENT `δ = r - V` (r counts DOWN to V) and an ASCENT `δ = V - r` (r counts UP to V).
        // Exactly one direction is a well-founded measure for a monotone move toward `good`; the other
        // wraps and fails harmlessly. For a relational good `a == b`, the two are `a - b` and `b - a`.
        let (primaries, not_good, good_nids): (
            Vec<(z3::ast::BV, z3::ast::BV)>,
            z3::ast::Bool,
            Vec<i64>,
        ) = match good_value {
            Some(v) => {
                let [r] = good_registers else { return false };
                let Some(&nid) = nid_map.get(r) else {
                    return false;
                };
                let (Some(rc), Some(rn)) = (view.curr_state(nid), view.next_state(nid)) else {
                    return false;
                };
                let w = rc.get_size();
                let vbv = z3::ast::BV::from_u64(v & mask(w), w);
                (
                    vec![
                        (rc.bvsub(&vbv), rn.bvsub(&vbv)), // descent δ = r - V
                        (vbv.bvsub(rc), vbv.bvsub(rn)),   // ascent  δ = V - r
                    ],
                    rc.eq(&vbv).not(),
                    vec![nid],
                )
            }
            None => {
                let [a, b] = good_registers else { return false };
                let (Some(&na), Some(&nb)) = (nid_map.get(a), nid_map.get(b)) else {
                    return false;
                };
                let (Some(ac), Some(an), Some(bc), Some(bn)) = (
                    view.curr_state(na),
                    view.next_state(na),
                    view.curr_state(nb),
                    view.next_state(nb),
                ) else {
                    return false;
                };
                if ac.get_size() != bc.get_size() {
                    return false;
                }
                (
                    vec![(ac.bvsub(bc), an.bvsub(bn)), (bc.bvsub(ac), bn.bvsub(an))],
                    ac.eq(bc).not(),
                    vec![na, nb],
                )
            }
        };

        // Lexicographic SECONDARY candidates: each OTHER state register's (curr, next) value — any BV is
        // a well-founded tiebreaker. Bounded + sorted-by-nid for a stable search order.
        const MAX_SECONDARIES: usize = 6;
        let mut sec_nids: Vec<i64> = view
            .state_curr
            .keys()
            .copied()
            .filter(|n| !good_nids.contains(n))
            .collect();
        sec_nids.sort_unstable();
        let secondaries: Vec<(z3::ast::BV, z3::ast::BV)> = sec_nids
            .iter()
            .take(MAX_SECONDARIES)
            .filter_map(|n| {
                Some((
                    view.state_curr.get(n)?.clone(),
                    view.state_next.get(n)?.clone(),
                ))
            })
            .collect();

        // Input-valuation enumeration setup (shared across measures). Only inputs in the state
        // registers' next-state cone are enumerated — out-of-cone inputs (an FPV backdoor, a dead
        // output's wide operand) are left FREE (sound: they cannot affect any δ_next) so they do not
        // swamp the enumeration's bit cap.
        let cone_inputs = inputs_in_next_cone(file, &good_nids);
        let input_bvs: Vec<z3::ast::BV> = view
            .inputs
            .iter()
            .filter(|(nid, _)| cone_inputs.contains(nid))
            .map(|(_, bv)| bv.clone())
            .collect();
        let total_input_bits: u32 = input_bvs.iter().map(|bv| bv.get_size()).sum();
        let can_enum = total_input_bits > 0 && total_input_bits <= 10;

        // For a candidate ranking measure, `non_dec` is "the measure did NOT strictly (lex-)decrease".
        //   - ALL-PATH (`AG AF good`): `transition ∧ ¬good ∧ non_dec` UNSAT ⇒ every input choice descends
        //     (down-counters, timers, deterministic nested loops). No quantifier.
        //   - SOME-PATH (`AG EF good`): ENUMERATE constant input valuations — if some fixed `v` makes
        //     `transition ∧ ¬good ∧ (inputs == v) ∧ non_dec` UNSAT, the constant-`v` path descends (a
        //     FIFO's read-gated drain). Non-quantified (z3's quantified-BV engine is version-flaky);
        //     capped at a few input bits (a drain is gated on narrow control inputs).
        // Either UNSAT ⇒ a strictly-descending, well-founded path reaches good ⇒ `AG EF good` (SOUND).
        let certifies = |non_dec: &z3::ast::Bool| -> bool {
            let mk_solver = || {
                let solver = z3::Solver::new();
                let mut params = z3::Params::new();
                params.set_u32("timeout", 5_000);
                solver.set_params(&params);
                solver
            };
            {
                let solver = mk_solver();
                solver.assert(&view.transition);
                solver.assert(&not_good);
                solver.assert(non_dec);
                if matches!(solver.check(), z3::SatResult::Unsat) {
                    return true;
                }
            }
            if can_enum {
                for v in 0u64..(1u64 << total_input_bits) {
                    let solver = mk_solver();
                    solver.assert(&view.transition);
                    solver.assert(&not_good);
                    let mut bit = 0u32;
                    for bv in &input_bvs {
                        let w = bv.get_size();
                        let m = if w >= 64 { u64::MAX } else { (1u64 << w) - 1 };
                        let vbv = z3::ast::BV::from_u64((v >> bit) & m, w);
                        let pin = bv.eq(&vbv);
                        solver.assert(&pin);
                        bit += w;
                    }
                    solver.assert(non_dec);
                    if matches!(solver.check(), z3::SatResult::Unsat) {
                        return true;
                    }
                }
            }
            false
        };

        // A ranking MEASURE is a list of components (curr, next), most-significant first. `non_decrease`
        // builds "the tuple did NOT strictly lex-decrease": fold least-significant up,
        //   acc_i = (next_i < curr_i) ∨ (next_i == curr_i ∧ acc_below),  base = false; then negate.
        let non_decrease = |measure: &[(&z3::ast::BV, &z3::ast::BV)]| -> z3::ast::Bool {
            let mut acc = z3::ast::Bool::from_bool(false);
            for &(curr, next) in measure.iter().rev() {
                let lt = next.bvult(curr);
                let eq = next.eq(curr);
                let tie = z3::ast::Bool::and(&[&eq, &acc]);
                acc = z3::ast::Bool::or(&[&lt, &tie]);
            }
            acc.not()
        };

        // Candidate measures, increasing complexity. MEASURE 1: single δ1. MEASURES 2..: each 2-tuple
        // `(δ1, s)` — decides a 2-level nested descent (δ1 holds while s counts down). FINAL: the FULL
        // tuple `(δ1, s1, …, sk)` over all secondaries in nid order — decides a k-LEVEL nested descent
        // (a 3-deep counter needs the 3-tuple; a 2-tuple over any single secondary fails). Lex order on
        // the tuple is well-founded, so the descent still reaches good (SOUND).
        for (d1_curr, d1_next) in &primaries {
            let d1 = (d1_curr, d1_next);
            if certifies(&non_decrease(&[d1])) {
                return true;
            }
            for (sc, sn) in &secondaries {
                if certifies(&non_decrease(&[d1, (sc, sn)])) {
                    return true;
                }
            }
            if secondaries.len() >= 2 {
                let mut full: Vec<(&z3::ast::BV, &z3::ast::BV)> = vec![d1];
                for (sc, sn) in &secondaries {
                    full.push((sc, sn));
                }
                if certifies(&non_decrease(&full)) {
                    return true;
                }
            }
        }
        false
    })
}

/// Decide recoverability `AG EF (good)` of `btor2_content`, where `good` is a single
/// register-comparison atom string (`"state_q == 3"`).
///
/// Returns the canonical [`PropertyVerdict`]: `Holds` (every reachable state can
/// reach `good`), `Violated` (a reachable trap cannot), or `Unknown` (neither the
/// exact engine nor the cube + `smt-hyper-must` escalation could decide). Errors only
/// when `good` is not a parseable atom.
///
/// P2 Slice 1 — the exact engine is tried first; on a definite verdict it is returned
/// as before. When the exact engine abstains (over the ~40-bit cone cap or an
/// unsupported construct), the property is **escalated** to
/// [`verify_recoverability_scalable`] (the cube + `smt-hyper-must` reduction), so the
/// νμ recoverability property decides beyond 40 bits. The public signature is
/// unchanged; callers that want extra abstraction predicates use
/// [`verify_recoverability_with_predicates`].
pub fn verify_recoverability(btor2_content: &str, good: &str) -> Result<PropertyVerdict, String> {
    verify_recoverability_with_predicates(btor2_content, good, &[])
}

/// [`verify_recoverability`] with optional extra abstraction predicates for the
/// cube-path escalation. `extra_predicates` refine the predicate-cube abstraction (they
/// help the `smt-hyper-must` path decide) but do NOT appear in the `AG EF good` formula
/// — only `good` does. When `extra_predicates` is empty this is exactly
/// [`verify_recoverability`]. The exact engine (tried first) ignores the extras; they
/// matter only if it abstains and the cube path runs.
pub fn verify_recoverability_with_predicates(
    btor2_content: &str,
    good: &str,
    extra_predicates: &[PredicateSpec],
) -> Result<PropertyVerdict, String> {
    // Validate the atom up front for a target-specific error message (the µ-parser
    // would otherwise report it as a formula-syntax error).
    parse_predicate_expr(good).map_err(|e| {
        format!("recoverability target `{good}` is not a register-comparison atom (`REG op VALUE`): {e:?}")
    })?;

    // AG EF good = nu Y. ((mu X. (good || <> X)) && [] Y). The exact engine reads the
    // raw `REG == VALUE` atom directly.
    let formula_str = format!("nu Y. ((mu X. (({good}) || <> X)) && [] Y)");
    let formula = mu_parser::parse(&formula_str)
        .map_err(|e| format!("building the AG EF formula for `{good}`: {e:?}"))?;

    match exact_symbolic_verdict(btor2_content, &formula) {
        // The exact engine decided (definite at every alternation depth per
        // Bruns–Godefroid) — return it unchanged; the exact-only behaviour is
        // preserved when the exact engine gives a definite verdict.
        Ok(v) => Ok(PropertyVerdict::from(v)),
        // Over the ~40-bit cone cap or an unsupported construct: escalate to the
        // cube + `smt-hyper-must` path so the property still decides at scale
        // (slice-5b safety-⊥ escalation mirror). The scalable path itself abstains
        // (Unknown) if it cannot decide, so this is never less sound than the old
        // `Err(_) => Unknown` abstention. The reason is logged, not dropped: an
        // OxiDD arena exhaustion looked exactly like a cone over the cap from the
        // outside, and the ladder's verdict then carried no trace of which engine
        // produced it (2026-10-06).
        Err(reason) => {
            tracing::warn!(
                target: "mununu::recoverability",
                %reason,
                "exact engine abstained on `AG EF {good}`; escalating to the scalable ladder"
            );
            // DIAG (never merge): the CI-only stack overflow sits on this path; the runner's
            // captured stderr is the only place the abstention REASON can be read.
            eprintln!("[diag overflow] exact engine abstained on `AG EF {good}`: {reason}");
            verify_recoverability_scalable(btor2_content, good, extra_predicates)
        }
    }
}

/// Phase 0 of the refined-verdicts plan (`.claude/plans/refined-verdicts-assumption-discovery.md`):
/// the recoverability verdict PLUS a structured [`VerdictRefinement`] carried alongside it. Today it
/// attaches a `Vacuous` witness (the target is never reachable) and the best-effort `bot_diagnosis`
/// "why ⊥" hint; later phases add the config partition (capability A) and discovered assumptions
/// (capability B) to the SAME object. The canonical [`PropertyVerdict`] is UNCHANGED — the refinement
/// is diagnostic-only and never turns a ⊥ into a bare `Holds` (an assumption/config scope is not a
/// sound unconditional verdict). Sidecar-free: operates on the same BTOR2 content the plain verb uses.
pub fn verify_recoverability_refined(
    btor2_content: &str,
    good: &str,
    extra_predicates: &[PredicateSpec],
    config_specs: &[(String, Vec<u64>)],
    discover_assumptions_flag: bool,
) -> (PropertyVerdict, VerdictRefinement) {
    let verdict = verify_recoverability_with_predicates(btor2_content, good, extra_predicates)
        .unwrap_or(PropertyVerdict::Unknown);
    let mut refinement = VerdictRefinement::default();

    // Config-partition (capability A) — partition the verdict over the config the recovery rides on.
    // When the caller NAMES config inputs (`--config-values`), partition over their values; otherwise
    // (bare `--refine`) AUTO-identify the config axis (the detected reset). Either way it is a concrete
    // decide per config, so it composes with any canonical verdict (it can even reveal a config that
    // breaks a HOLDS). Named inputs are reported as asked, one cell included (#634); the auto axis
    // reports only a config-DEPENDENT partition. `None` ⇒ no reset detected, or over the cap.
    let part = if config_specs.is_empty() {
        auto_config_partition(btor2_content, good)
    } else {
        config_partition(btor2_content, good, config_specs)
    };
    if let Some(part) = part {
        refinement.config_partition = Some(part);
    }

    // Vacuity probe — only for a non-`Holds` verdict (a `Holds` target is trivially reachable). A
    // sound `Unreachable` from the reachability portfolio (never emitted on free-init state) means
    // the target is never entered ⇒ the `AG EF(good)` verdict is degenerate.
    if verdict != PropertyVerdict::Holds
        && let Some(vac) = probe_vacuous(btor2_content, good)
    {
        refinement.vacuous = Some(vac);
    }

    // Structural "why ⊥ / what would decide it" hint — only meaningful when genuinely undecided.
    if verdict == PropertyVerdict::Unknown {
        let diag = diagnose_recoverability_bot(btor2_content, good);
        if !diag.is_empty() {
            refinement.bot_diagnosis = Some(diag);
        }
    }

    // Assumption discovery (capability B) — the caller opted in (it costs runs). CONDITIONAL-ONLY: the
    // discovered `holds_under` NEVER changes the canonical verdict (assumptions are not monotone for
    // `AG EF`, so a `HoldsUnder(φ)` does not transfer unconditionally).
    if discover_assumptions_flag {
        if verdict != PropertyVerdict::Holds {
            // Base model does not hold ⇒ search for an enabling assumption over the base model. First
            // the cheap memoryless single-input holds (slice 1); if none, fall back to the symbolic
            // POSITIONAL environment strategy (Phase 2b) — it finds a per-state recovery discipline no
            // constant hold expresses (νX. EF(good) ∧ ◇X over the bit-blasted BDD).
            let assumptions = discover_assumptions(btor2_content, good, &[]);
            if !assumptions.is_empty() {
                refinement.holds_under = assumptions;
            } else if let Some(env) = synthesize_env_strategy_assumption(btor2_content, good) {
                refinement.holds_under = vec![env];
            }
        } else if let Some(part) = &refinement.config_partition
            && let Some(operational) = part.violated.first()
        {
            // A→B COMPOSITION: the base HOLDS (e.g. a free reset trivially recovers `AG EF`), but
            // `config_partition` found an operational config cell that is VIOLATED. Discover the
            // enabling assumption WITHIN that operational config — capability A localizes the bad
            // config, capability B explains how to still recover under it (e.g. free-reset HOLDS, but
            // operationally (`rst=1`) it traps unless a fault strobe is held low).
            //
            // Pinning the reset inactive removes the async reset that establishes the FSM's start state,
            // so on real RTL the operational model would have a HAVOC initial state (it could START in the
            // trap ⇒ no constant hold recovers). `operational_reset_init_model` injects the post-reset
            // init first, so the search asks the meaningful "from the reset state, operationally" question.
            let op_model = operational_reset_init_model(btor2_content);
            let assumptions = discover_assumptions(&op_model, good, operational);
            if !assumptions.is_empty() {
                refinement.holds_under = assumptions;
            }
        }
    }
    (verdict, refinement)
}

/// Parse `--config-values`-style entries `"NAME=v1,v2,…"` into config-partition specs
/// `(input_name, [values])`. Reuses the decimal-value convention; a malformed entry is an error.
pub fn parse_config_value_specs(entries: &[String]) -> Result<Vec<(String, Vec<u64>)>, String> {
    entries
        .iter()
        .map(|e| {
            let (name, vals) = e
                .split_once('=')
                .ok_or_else(|| format!("--config-values `{e}`: expected `NAME=v1,v2,...`"))?;
            let name = name.trim();
            if name.is_empty() {
                return Err(format!("--config-values `{e}`: empty signal name"));
            }
            let values = vals
                .split(',')
                .map(|v| {
                    v.trim()
                        .parse::<u64>()
                        .map_err(|_| format!("--config-values `{e}`: `{v}` is not a decimal value"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if values.is_empty() {
                return Err(format!("--config-values `{e}`: no values listed"));
            }
            Ok((name.to_string(), values))
        })
        .collect()
}

/// SOUND vacuity probe: is `good` (a `REG == VALUE` atom) reachable from the initial state? Emits a
/// `bad = (REG == VALUE)` monitor — via the `AG(REG != VALUE)` monitor, whose `bad` is exactly
/// `!(REG != VALUE) = (REG == VALUE) = good` — and asks the reachability portfolio. A sound
/// `Unreachable` ⇒ `good` is never reached ⇒ vacuous. `None` when the atom is not a `REG == VALUE`
/// equality, or the probe cannot run (a hint is never load-bearing — never changes a verdict).
fn probe_vacuous(btor2_content: &str, good: &str) -> Option<VacuityWitness> {
    use crate::adapter::btor2::bad_monitor::emit_ag_state_atom_monitor;
    use crate::adapter::reach_portfolio::{ReachVerdict, decide_reach_portfolio};
    let (register, value) = match parse_predicate_expr(good).ok()? {
        PredicateExpr::Cmp {
            register,
            op: CmpOp::Eq,
            value,
        } => (register, value),
        _ => return None, // relational / inequality target: no vacuity determination here
    };
    let monitored =
        emit_ag_state_atom_monitor(btor2_content, &register, CmpOp::Ne, value as u128, false)
            .ok()?;
    let file = crate::adapter::btor2::parser::parse(&monitored).ok()?;
    match decide_reach_portfolio(&file).verdict {
        ReachVerdict::Unreachable => Some(VacuityWitness {
            good_unreachable: true,
            note: format!(
                "`{good}` is never reachable from the initial state — `AG EF({good})` is degenerate \
                 (the recovery target is never entered); the verdict is vacuous, not a meaningful decide"
            ),
        }),
        _ => None, // Reachable, or Unknown/Contradiction ⇒ not (soundly) vacuous
    }
}

/// The MANDATORY non-vacuity gate for a discovered `HoldsUnder` (the i2c I1 / P1.2 lesson): an
/// `AG EF(REG == VALUE)` HOLDS is meaningful ONLY if `good` is genuinely REACHED *and* genuinely LEFT
/// under the assumption. Otherwise it is trivially true — `good` unreachable (nothing to recover to),
/// or `good` stuck-true (the system never leaves it) — and the φ is worthless. Both directions via the
/// sound [`decide_reach_portfolio`]; a `Reachable` on each is required. Any non-`Reachable` (including
/// `Unknown`) fails the gate — we NEVER report a `HoldsUnder` we cannot certify is non-vacuous.
fn good_nonvacuous_under(content: &str, register: &str, value: u64) -> bool {
    use crate::adapter::btor2::bad_monitor::emit_ag_state_atom_monitor;
    use crate::adapter::reach_portfolio::{ReachVerdict, decide_reach_portfolio};
    let reachable = |op: CmpOp| -> bool {
        let Ok(m) = emit_ag_state_atom_monitor(content, register, op, value as u128, false) else {
            return false;
        };
        let Ok(f) = crate::adapter::btor2::parser::parse(&m) else {
            return false;
        };
        matches!(decide_reach_portfolio(&f).verdict, ReachVerdict::Reachable)
    };
    // good REACHED: `EF(REG == VALUE)` — the `AG(REG != VALUE)` monitor's `bad` is `(REG == VALUE)`.
    // good LEFT (not stuck-true): `EF(REG != VALUE)` — the `AG(REG == VALUE)` monitor's `bad` is `(REG != VALUE)`.
    reachable(CmpOp::Ne) && reachable(CmpOp::Eq)
}

/// Capability B (refined-verdicts Phase 2) — DISCOVER an environment assumption φ under which an
/// otherwise ⊥/VIOLATED `AG EF good` becomes a NON-VACUOUS HOLDS. Slice 1 searches single-input HOLDS
/// assumptions: for each NARROW named primary input, pin it to each value, re-decide with the exact
/// engine, and keep the pins that yield a HOLDS whose `good` passes the non-vacuity gate
/// ([`good_nonvacuous_under`]). Each returned [`DiscoveredAssumption`] is CONDITIONAL — the caller's
/// canonical verdict is UNCHANGED (assumptions are not monotone for `AG EF`; a `HoldsUnder(φ)` does not
/// transfer to the unconstrained system). Conjunctive / wide-input / temporal (`ResetEventually`) /
/// env-strategy assumptions are deferred to later Phase-2 slices; an empty result is the honest
/// "no single-input assumption found."
///
/// **Engine (§16):** `exact-symbolic` full-state ROBDD (OxiDD) on each [`pin_inputs_to_constants`]
/// concrete model (2-valued sound per pin) + the `decide_reach_portfolio` reachability engine for the
/// non-vacuity gate. Tool-free, sidecar-free. A candidate cap + a total wall-clock budget
/// (`MUNUNU_ASSUMPTION_BUDGET_MS`, default 90 s) bound the enumeration on wide designs.
/// Build the faithful OPERATIONAL model for the A→B composition: an async-reset design derives its
/// start state FROM the reset, so pinning the reset inactive (to ask the operational question) would
/// leave a HAVOC initial state — the trap becomes reachable as an initial state and no constant hold
/// can recover. [`inject_reset_init`] simulates one reset-asserted cycle and appends the post-reset
/// `init` lines, so the operational search starts from the reset state. No-op when the design has no
/// detected reset or already carries authoritative `init` lines (then it is returned unchanged).
fn operational_reset_init_model(btor2_content: &str) -> String {
    let Ok(file) = crate::adapter::btor2::parser::parse(btor2_content) else {
        return btor2_content.to_string();
    };
    let symbols = crate::adapter::btor2::parser::collect_symbols(&file);
    let resets = crate::adapter::fsm_scan::detect_resets(&file, &symbols);
    crate::adapter::btor2::reset_init::inject_reset_init(btor2_content, &resets)
        .unwrap_or_else(|_| btor2_content.to_string())
}

fn discover_assumptions(
    btor2_content: &str,
    good: &str,
    context: &[(String, u64)],
) -> Vec<DiscoveredAssumption> {
    use crate::adapter::btor2::ast::Node;
    use crate::adapter::btor2::pin::pin_inputs_to_constants;
    const MAX_INPUT_BITS: u32 = 3; // ≤ 8 values per input — a narrow control/enable, not a datapath
    const MAX_CANDIDATE_PINS: usize = 48;
    let budget_ms: u128 = std::env::var("MUNUNU_ASSUMPTION_BUDGET_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(90_000);
    let start = std::time::Instant::now();

    // Only `REG == VALUE` targets — the non-vacuity gate needs the register + value.
    let (register, value) = match parse_predicate_expr(good).ok() {
        Some(PredicateExpr::Cmp {
            register,
            op: CmpOp::Eq,
            value,
        }) => (register, value),
        _ => return Vec::new(),
    };
    // `context` = config pins applied FIRST (the A→B composition: the operational config the recovery
    // rides on, e.g. `rst_ni=1` from a VIOLATED `config_partition` cell). Enumerate candidates over the
    // context-pinned model so the context inputs are already constants (not re-searched), and prefix each
    // discovered φ with the context so the reported assumption is the FULL enabling condition.
    let base = if context.is_empty() {
        btor2_content.to_string()
    } else {
        pin_inputs_to_constants(btor2_content, context).0
    };
    let ctx_prefix: String = context
        .iter()
        .map(|(n, v)| format!("{n} == {v} && "))
        .collect();
    let engine = if context.is_empty() {
        "exact-symbolic under single-input hold".to_string()
    } else {
        "exact-symbolic under config-scoped input hold".to_string()
    };
    let Ok(file) = crate::adapter::btor2::parser::parse(&base) else {
        return Vec::new();
    };
    let symbols = crate::adapter::btor2::parser::collect_symbols(&file);

    // Candidate hold-axes = the design's NARROW named primary inputs. A pin that does not affect the
    // recovery cone simply won't flip the verdict (so it is never reported) — the re-verify IS the
    // relevance filter — so no explicit cone restriction is needed here.
    let mut candidates: Vec<(String, u32)> = Vec::new();
    for line in &file.lines {
        if let Node::Input { sort, .. } = &line.node
            && let Some(name) = symbols.get(&line.nid)
            && let Some(w) = crate::adapter::btor2::parser::bv_width(&file, *sort)
            && w <= MAX_INPUT_BITS
        {
            candidates.push((name.clone(), w));
        }
    }
    candidates.sort();
    candidates.dedup();

    let mut found: Vec<DiscoveredAssumption> = Vec::new();
    let mut pins_tried = 0usize;
    'outer: for (name, w) in candidates {
        for c in 0..(1u64 << w) {
            if pins_tried >= MAX_CANDIDATE_PINS || start.elapsed().as_millis() > budget_ms {
                break 'outer;
            }
            pins_tried += 1;
            let (pinned, _) = pin_inputs_to_constants(&base, &[(name.clone(), c)]);
            // SOUNDNESS: each pinned model is CONCRETE ⇒ exact-symbolic on it is 2-valued sound; the
            // non-vacuity gate rejects a HOLDS whose `good` is unreachable or stuck-true. The REPORT
            // stays conditional-only (the caller never lifts it to a bare Holds).
            if verify_recoverability_with_predicates(&pinned, good, &[]).ok()
                == Some(PropertyVerdict::Holds)
                && good_nonvacuous_under(&pinned, &register, value)
            {
                found.push(DiscoveredAssumption {
                    phi: format!("{ctx_prefix}{name} == {c}"),
                    kind: AssumptionKind::InputHold,
                    non_vacuous: true,
                    engine: engine.clone(),
                });
            }
        }
    }
    found
}

/// Capability B, Phase 2b — synthesize a symbolic POSITIONAL environment strategy that maintains
/// `AG EF good`, reported as a [`DiscoveredAssumption`] of [`AssumptionKind::EnvStrategy`]. The
/// fallback when slice-1's memoryless single-input holds find nothing: the recovery discipline may be
/// per-state (a positional strategy), which a constant hold cannot express. `None` when the exact
/// engine cannot build the model (over-cap / array / input-atom → `Inapplicable`), when no strategy
/// maintains it (`NotMaintainable` — the adversary forces a permanent trap), or when the non-vacuity
/// gate fails. CONDITIONAL-ONLY: the caller never lifts this to a bare `Holds`.
///
/// **Engine (§16):** `exact-symbolic` env-strategy — [`exact_env_strategy`], the `νX. EF(good) ∧ ◇X`
/// safety game over the bit-blasted BDD (`◇` = ∃input = the environment's move). Tool-free.
fn synthesize_env_strategy_assumption(
    btor2_content: &str,
    good: &str,
) -> Option<DiscoveredAssumption> {
    use crate::adapter::btor2::symbolic_bitblast::{EnvStrategyOutcome, exact_env_strategy};
    // `exact_env_strategy` returns `Maintainable` ONLY when it is strategy-intrinsically non-vacuous
    // (the environment can leave `good` within the maintained region — see its `ef_within` gate), so no
    // further free-model non-vacuity check is needed here.
    let EnvStrategyOutcome::Maintainable { first_move, .. } =
        exact_env_strategy(btor2_content, good).ok()?
    else {
        return None;
    };
    let moves = if first_move.is_empty() {
        String::new()
    } else {
        let m: Vec<String> = first_move.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!(" (e.g. first move {})", m.join(", "))
    };
    Some(DiscoveredAssumption {
        phi: format!(
            "environment strategy maintains recovery — stay in the recoverable region{moves}"
        ),
        kind: AssumptionKind::EnvStrategy,
        non_vacuous: true,
        engine: "exact-symbolic env-strategy (νX. EF(good) ∧ ◇X)".to_string(),
    })
}

/// Capability B, Phase 2c (TWO-PLAYER) — discover an ENVIRONMENT ASSUMPTION under which the
/// controllable-reachability GAME to `good` becomes REALIZABLE. When the controller cannot force `good`
/// against a free adversary (`exact_two_player_strategy` returns an `EnvironmentCounterstrategy`), search
/// for a constraint on the ENVIRONMENT (non-controllable) inputs under which the controller WINS — the
/// assume-guarantee wedge: `G` is unrealizable, but `A ⇒ G` is realizable ("the boot handshake completes
/// IF the acknowledging module eventually acks"). Each returned [`DiscoveredAssumption`] is CONDITIONAL
/// (the caller's canonical verdict is unchanged). Empty when the game is ALREADY realizable (no
/// assumption needed — the soundness/false-positive control), when `good` is not `REG == VALUE`, or when
/// no single narrow env-input hold wins.
///
/// The environment COUNTERSTRATEGY is the witness: the env inputs it FORCES are the blockers, so those
/// axes are searched FIRST — a hold that constrains the environment away from its blocking discipline is
/// the natural assumption. Unlike the 1-player [`synthesize_env_strategy_assumption`] (which asks whether
/// the environment, owning ALL inputs, can MAINTAIN recovery), this splits inputs into controller vs
/// environment and asks whether CONSTRAINING the adversary lets the CONTROLLER win — the generalization
/// the refined-verdicts plan flagged, unblocked by the two-player CPre (`exact_model_partitioned`).
///
/// **Engine (§16):** `exact-symbolic` ROBDD two-player game ([`exact_two_player_strategy`], `∀env ∃ctrl`
/// CPre) on each [`pin_inputs_to_constants`] env-pinned concrete model (2-valued sound per pin) + the
/// reach portfolio for the non-vacuity gate. A candidate cap + a wall-clock budget bound the search.
pub fn discover_game_env_assumption(
    btor2_content: &str,
    good: &str,
    controllable: &[&str],
) -> Vec<DiscoveredAssumption> {
    use crate::adapter::btor2::ast::Node;
    use crate::adapter::btor2::pin::pin_inputs_to_constants;
    use crate::adapter::btor2::symbolic_bitblast::{
        TwoPlayerStrategy, exact_two_player_reach_realizable, exact_two_player_strategy,
    };
    const MAX_INPUT_BITS: u32 = 3; // ≤ 8 values — a narrow control/ack/enable, not a datapath
    const MAX_CANDIDATE_PINS: usize = 48;
    let budget_ms: u128 = std::env::var("MUNUNU_ASSUMPTION_BUDGET_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(90_000);
    let start = std::time::Instant::now();

    // `good` must be `REG == VALUE` (a state register OR a named combinational output like `full_o == 1`)
    // — the non-vacuity gate needs the register/output + value.
    let (register, value) = match parse_predicate_expr(good).ok() {
        Some(PredicateExpr::Cmp {
            register,
            op: CmpOp::Eq,
            value,
        }) => (register, value),
        _ => return Vec::new(),
    };

    // Only search when the game is genuinely UNREALIZABLE — a realizable game needs no assumption (the
    // false-positive control: discovery must not fabricate an enabling φ for a controller that already
    // wins). Uses the FORMULA verdict (`exact_two_player_reach_realizable`), so `good` may be a
    // combinational OUTPUT / relational target (a FIFO's `full_o`), not only a state register.
    match exact_two_player_reach_realizable(btor2_content, good, controllable) {
        Ok(false) => {}         // unrealizable → search for an enabling assumption
        _ => return Vec::new(), // realizable, or `good` not resolvable / over-cap
    }
    // The env COUNTERSTRATEGY (when extractable — a state-register target) names the blocking env inputs,
    // searched FIRST. Best-effort: a combinational-output/relational target has no state-indexed strategy,
    // so `priority` is empty there and every candidate is searched.
    let priority: std::collections::HashSet<String> =
        match exact_two_player_strategy(btor2_content, good, controllable) {
            Ok(TwoPlayerStrategy::EnvironmentCounterstrategy(p)) => p
                .entries
                .iter()
                .flat_map(|e| e.forced_inputs.keys().cloned())
                .collect(),
            _ => std::collections::HashSet::new(),
        };

    let ctrl_set: std::collections::HashSet<&str> = controllable.iter().copied().collect();
    let Ok(file) = crate::adapter::btor2::parser::parse(btor2_content) else {
        return Vec::new();
    };
    let symbols = crate::adapter::btor2::parser::collect_symbols(&file);
    // Candidate assumption axes = the design's NARROW ENVIRONMENT primary inputs (narrow, non-controllable).
    let mut candidates: Vec<(String, u32)> = Vec::new();
    for line in &file.lines {
        if let Node::Input { sort, .. } = &line.node
            && let Some(name) = symbols.get(&line.nid)
            && !ctrl_set.contains(name.as_str())
            && let Some(w) = crate::adapter::btor2::parser::bv_width(&file, *sort)
            && w <= MAX_INPUT_BITS
        {
            candidates.push((name.clone(), w));
        }
    }
    candidates.sort();
    candidates.dedup();
    // Search the counterstrategy-forced (blocking) env inputs FIRST — the witness guides the search.
    candidates.sort_by_key(|(n, _)| (!priority.contains(n), n.clone()));

    let mut found: Vec<DiscoveredAssumption> = Vec::new();
    let mut pins_tried = 0usize;
    'outer: for (name, w) in candidates {
        for c in 0..(1u64 << w) {
            if pins_tried >= MAX_CANDIDATE_PINS || start.elapsed().as_millis() > budget_ms {
                break 'outer;
            }
            pins_tried += 1;
            let (pinned, _) = pin_inputs_to_constants(btor2_content, &[(name.clone(), c)]);
            // SOUNDNESS: the env input is pinned to a CONSTANT ⇒ the two-player game on the pinned model
            // is exact/2-valued sound; the non-vacuity gate rejects a realizable-but-vacuous hold (good
            // unreachable or stuck-true under the assumption). The report stays CONDITIONAL — the caller
            // never lifts it to an unconditional realizable.
            let realizable =
                exact_two_player_reach_realizable(&pinned, good, controllable) == Ok(true);
            if realizable && good_nonvacuous_under(&pinned, &register, value) {
                found.push(DiscoveredAssumption {
                    phi: format!("{name} == {c}"),
                    kind: AssumptionKind::InputHold,
                    non_vacuous: true,
                    engine: "two-player game realizable under env-input hold".to_string(),
                });
            }
        }
    }
    // When NO single hold wins (multiple independent adversarial blockers — the edn/otbn case), fall back
    // to the CONJUNCTIVE slice: a minimal SET of env-input holds under which the controller wins.
    if found.is_empty()
        && let Some(conj) = discover_game_env_conjunction(btor2_content, good, controllable)
    {
        found.push(conj);
    }
    found
}

/// Capability B, Phase 2c (FAIRNESS / liveness) — when the RECURRENCE (Büchi) game `GF good` is
/// UNREALIZABLE, find a LIVENESS environment assumption `GF(in == v)` (the environment input holds `v`
/// INFINITELY OFTEN) under which the controller wins `GF good` — the GR(1) 1-pair objective
/// `GF a → GF good`. Distinct from [`discover_game_env_assumption`]'s `InputHold` (a SAFETY hold
/// `G(a)`): fairness is strictly WEAKER — the environment may violate `a` finitely often.
///
/// **Scope (measure-first, honest).** Fairness rescues RESPONSE / handshake shapes ("if the environment
/// eventually acks, the controller eventually completes"), NOT RATE / accumulation targets: a bounded
/// buffer's `GF(full)` is NOT fairness-rescuable, because `GF(¬drain)` (drain paused infinitely often)
/// still lets the environment net-drain (drain 99, pause 1) — the accumulation never completes. So this
/// correctly returns ∅ on a rate machine (the FIFO) and finds the rescue on a response FSM.
///
/// **Engine (§16):** `exact-symbolic` ROBDD — [`exact_two_player_gr1_realizable`] decides the
/// state-fairness GR(1) νμν on the assumption-LATCHED model (a sound encoding of the input-level
/// `GF a`); the reach-portfolio non-vacuity gate rejects a vacuous rescue (`good` unreachable or
/// stuck-true). Bounded: narrow env inputs (≤ 3 bits) × values, a pin count cap, and a wall-clock
/// budget. The report is CONDITIONAL — the caller never lifts it to an unconditional realizable.
pub fn discover_game_fairness_assumption(
    btor2_content: &str,
    good: &str,
    controllable: &[&str],
) -> Vec<DiscoveredAssumption> {
    use crate::adapter::btor2::ast::Node;
    use crate::adapter::btor2::concrete_oracle::OracleAtom;
    use crate::adapter::btor2::symbolic_bitblast::{
        exact_two_player_buchi_realizable, exact_two_player_gr1_realizable,
    };
    const MAX_INPUT_BITS: u32 = 3; // ≤ 8 values — a narrow ack/enable, not a datapath
    const MAX_CANDIDATE_PINS: usize = 48;
    let budget_ms: u128 = std::env::var("MUNUNU_ASSUMPTION_BUDGET_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(90_000);
    let start = std::time::Instant::now();

    // `good` must be `REG == VALUE` (a state register OR a named combinational output) — the non-vacuity
    // gate needs the register/output + value.
    let (register, value) = match parse_predicate_expr(good).ok() {
        Some(PredicateExpr::Cmp {
            register,
            op: CmpOp::Eq,
            value,
        }) => (register, value),
        _ => return Vec::new(),
    };

    // Only search when the RECURRENCE game is genuinely UNREALIZABLE (a realizable Büchi game needs no
    // assumption — the false-positive control). This is also what makes a discovered φ a GENUINE rescue:
    // any realizable GR(1) under `GF a` (given Büchi is ⊥) truly uses the assumption.
    match exact_two_player_buchi_realizable(btor2_content, good, controllable) {
        Ok(false) => {}         // unrealizable → search for an enabling fairness assumption
        _ => return Vec::new(), // realizable, or `good` not resolvable / over-cap
    }

    let ctrl_set: std::collections::HashSet<&str> = controllable.iter().copied().collect();
    let Ok(file) = crate::adapter::btor2::parser::parse(btor2_content) else {
        return Vec::new();
    };
    let symbols = crate::adapter::btor2::parser::collect_symbols(&file);
    // Candidate assumption axes = the design's NARROW ENVIRONMENT primary inputs.
    let mut candidates: Vec<(String, u32)> = Vec::new();
    for line in &file.lines {
        if let Node::Input { sort, .. } = &line.node
            && let Some(name) = symbols.get(&line.nid)
            && !ctrl_set.contains(name.as_str())
            && let Some(w) = crate::adapter::btor2::parser::bv_width(&file, *sort)
            && w <= MAX_INPUT_BITS
        {
            candidates.push((name.clone(), w));
        }
    }
    candidates.sort();
    candidates.dedup();

    let mut found: Vec<DiscoveredAssumption> = Vec::new();
    let mut tried = 0usize;
    'outer: for (name, w) in candidates {
        for c in 0..(1u64 << w) {
            if tried >= MAX_CANDIDATE_PINS || start.elapsed().as_millis() > budget_ms {
                break 'outer;
            }
            tried += 1;
            let atom = OracleAtom::new(name.clone(), CmpOp::Eq, c as u128);
            // SOUNDNESS: GR(1) 1-pair on the assumption-latched model (exact/2-valued sound); the
            // non-vacuity gate rejects a realizable-but-vacuous rescue (good unreachable or stuck-true).
            // CONDITIONAL — the caller never lifts it to an unconditional realizable.
            let realizable =
                exact_two_player_gr1_realizable(btor2_content, good, &atom, controllable)
                    == Ok(true);
            if realizable && good_nonvacuous_under(btor2_content, &register, value) {
                found.push(DiscoveredAssumption {
                    phi: format!("GF({name} == {c})"),
                    kind: AssumptionKind::InputFairness,
                    non_vacuous: true,
                    engine: "two-player GR(1) game realizable under env-input fairness".to_string(),
                });
            }
        }
    }
    // When NO single fairness assumption wins (multiple independent liveness blockers), fall back to the
    // CONJUNCTIVE slice: a minimal SET of `GF(e_i == v_i)` under which the controller wins.
    if found.is_empty()
        && let Some(conj) = discover_game_fairness_conjunction(btor2_content, good, controllable)
    {
        found.push(conj);
    }
    found
}

/// Capability B, Phase 2c (FAIRNESS, CONJUNCTIVE) — when NO single `GF(e_i == v_i)` rescues the
/// unrealizable recurrence game (multiple INDEPENDENT liveness blockers), find a MINIMAL CONJUNCTION of
/// fairness assumptions `GF(e_1 == v_1) ∧ … ∧ GF(e_k == v_k)` under which the controller wins
/// `(⋀_i GF a_i) → GF good` (the multi-pair GR(1) objective). The fairness analog of
/// [`discover_game_env_conjunction`] (which pins env inputs to CONSTANTS for a reach/safety `A ⇒ G`);
/// here each `e_i` stays free EXCEPT that it must be `v_i` INFINITELY OFTEN.
///
/// Returns ONE minimal conjunction (≥2 assumptions), or `None` (already realizable, `good` not
/// `REG == VALUE`, no winning value-combination within the caps, or it minimizes to a single assumption
/// — that is the [`discover_game_fairness_assumption`] single-`GF` slice's job).
///
/// **Engine (§16):** `exact-symbolic` ROBDD multi-pair GR(1)
/// ([`crate::adapter::btor2::symbolic_bitblast::exact_two_player_gr1_conjunction_realizable`]) over the
/// `k`-latch model + the reach-portfolio non-vacuity gate. Bounded: candidate env inputs are restricted
/// to `good`'s cone (`cone_leaf_nids`); the winning-combination search caps at `MAX_CONJ_BITS` total env
/// bits (the νμ over `k` latches is pricier than a reach pin, so the cap is tighter than the reach
/// slice's) and a wall-clock budget.
fn discover_game_fairness_conjunction(
    btor2_content: &str,
    good: &str,
    controllable: &[&str],
) -> Option<DiscoveredAssumption> {
    use crate::adapter::btor2::ast::Node;
    use crate::adapter::btor2::concrete_oracle::OracleAtom;
    use crate::adapter::btor2::dep_graph::cone_leaf_nids;
    use crate::adapter::btor2::symbolic_bitblast::{
        exact_two_player_buchi_realizable, exact_two_player_gr1_conjunction_realizable,
    };
    const MAX_INPUT_BITS: u32 = 3;
    const MAX_CONJ_BITS: u32 = 6; // ≤ 64 value-combinations; the νμ over k latches is pricier than a pin
    let budget_ms: u128 = std::env::var("MUNUNU_ASSUMPTION_BUDGET_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(90_000);
    let start = std::time::Instant::now();

    let (register, value) = match parse_predicate_expr(good).ok() {
        Some(PredicateExpr::Cmp {
            register,
            op: CmpOp::Eq,
            value,
        }) => (register, value),
        _ => return None,
    };
    // Only when the RECURRENCE game is genuinely unrealizable.
    if exact_two_player_buchi_realizable(btor2_content, good, controllable) != Ok(false) {
        return None;
    }
    // Non-vacuity is a property of the DESIGN (good reachable AND avoidable), independent of the
    // assumption — check it once. A dead/stuck target yields no meaningful conjunction.
    if !good_nonvacuous_under(btor2_content, &register, value) {
        return None;
    }
    let ctrl_set: std::collections::HashSet<&str> = controllable.iter().copied().collect();
    let Ok(file) = crate::adapter::btor2::parser::parse(btor2_content) else {
        return None;
    };
    let symbols = crate::adapter::btor2::parser::collect_symbols(&file);
    // Candidate axes = narrow NON-controllable primary inputs. Prefer `good`'s cone (bounds the product on
    // wide-env designs); but `cone_leaf_nids` is a COMBINATIONAL cone, so for an OUTPUT / relational target
    // it EXCLUDES an env input that only drives the target's state-register's NEXT logic (e.g. an ack that
    // returns an FSM to its home state — `b2r_ack` for `sdrc_req_gen`'s `req_ack`). When the cone yields
    // < 2 axes, fall back to ALL narrow non-controllable inputs (the single-`GF` candidate set, which is
    // cone-free); the width cap still bounds the search.
    let build_axes = |restrict_to_cone: bool| -> Vec<(String, u32)> {
        let cone = restrict_to_cone.then(|| cone_leaf_nids(&file, std::slice::from_ref(&register)));
        let mut axes: Vec<(String, u32)> = Vec::new();
        for line in &file.lines {
            if let Node::Input { sort, .. } = &line.node
                && cone.as_ref().is_none_or(|c| c.contains(&line.nid))
                && let Some(name) = symbols.get(&line.nid)
                && !ctrl_set.contains(name.as_str())
                && let Some(w) = crate::adapter::btor2::parser::bv_width(&file, *sort)
                && w <= MAX_INPUT_BITS
            {
                axes.push((name.clone(), w));
            }
        }
        axes.sort();
        axes.dedup();
        axes
    };
    let mut axes = build_axes(true);
    if axes.len() < 2 {
        axes = build_axes(false);
    }
    let total_bits: u32 = axes.iter().map(|(_, w)| *w).sum();
    if axes.len() < 2 || total_bits > MAX_CONJ_BITS {
        return None; // < 2 axes = not a conjunction; too wide = abstain
    }

    // `wins`: the SUBSET `kept` of axes, each assumed `GF(e_j == vals[j])`, makes the multi-pair GR(1)
    // game realizable. (Non-vacuity already checked above — it does not depend on the assumption.)
    let wins = |kept: &[usize], vals: &[u64]| -> bool {
        let assumes: Vec<OracleAtom> = kept
            .iter()
            .map(|&j| OracleAtom::new(axes[j].0.clone(), CmpOp::Eq, vals[j] as u128))
            .collect();
        exact_two_player_gr1_conjunction_realizable(btor2_content, good, &assumes, controllable)
            == Ok(true)
    };

    // Search the product of the axes' value ranges for a WINNING full assumption set (all axes assumed).
    let ranges: Vec<u64> = axes.iter().map(|(_, w)| 1u64 << w).collect();
    let total_combos: u64 = ranges.iter().product();
    let all: Vec<usize> = (0..axes.len()).collect();
    let mut vals: Option<Vec<u64>> = None;
    for idx in 0..total_combos {
        if start.elapsed().as_millis() > budget_ms {
            break;
        }
        let mut rem = idx;
        let candidate: Vec<u64> = ranges
            .iter()
            .map(|r| {
                let v = rem % r;
                rem /= r;
                v
            })
            .collect();
        if wins(&all, &candidate) {
            vals = Some(candidate);
            break;
        }
    }
    let vals = vals?;

    // MINIMIZE: greedily drop any assumption the controller wins WITHOUT (that env input can stay fully
    // adversarial).
    let mut needed: Vec<usize> = (0..axes.len()).collect();
    for i in 0..axes.len() {
        let trial: Vec<usize> = needed.iter().copied().filter(|&j| j != i).collect();
        if wins(&trial, &vals) {
            needed = trial;
        }
    }
    if needed.len() < 2 {
        return None; // reduced to a single fairness assumption — the single-`GF` slice's job
    }
    let phi = needed
        .iter()
        .map(|&j| format!("GF({} == {})", axes[j].0, vals[j]))
        .collect::<Vec<_>>()
        .join(" && ");
    Some(DiscoveredAssumption {
        phi,
        kind: AssumptionKind::InputFairnessConjunction,
        non_vacuous: true,
        engine: "two-player multi-pair GR(1) game realizable under env-input fairness conjunction"
            .to_string(),
    })
}

/// Capability B, Phase 2c (CONJUNCTIVE) — when NO single environment-input hold makes the game realizable
/// (the design has MULTIPLE independent adversarial blockers, e.g. the OpenTitan edn/otbn boot handshakes:
/// the environment can block by withholding the ack OR escalating OR requesting a wipe), find a MINIMAL
/// CONJUNCTION of env-input holds under which the controller WINS — `A = (e_i == v_i ∧ …) ⇒ G` realizable.
/// Returns ONE minimal conjunction (≥2 inputs), or `None` (already realizable, `good` not `REG == VALUE`,
/// no winning combination within the width/budget caps, or it minimizes to a single hold — that is the
/// [`discover_game_env_assumption`] single-hold slice's job).
///
/// Pinning a set of env inputs to constants makes them non-adversarial; pinning ALL cone-relevant env
/// inputs to a WINNING combination reduces the game to 1-player controllable reachability; the minimal
/// such set (greedily drop any pin the controller wins without) is the assume-guarantee assumption.
///
/// **Engine (§16):** `exact-symbolic` ROBDD two-player game ([`exact_two_player_strategy`]) on each
/// [`pin_inputs_to_constants`] model + the reach-portfolio non-vacuity gate. Bounded: candidate env inputs
/// are restricted to `good`'s cone-of-influence (`cone_leaf_nids` — excludes dangling clocks); the
/// winning-combination search caps at `MAX_CONJ_BITS` total env bits (abstain if wider) and a wall-clock
/// budget.
fn discover_game_env_conjunction(
    btor2_content: &str,
    good: &str,
    controllable: &[&str],
) -> Option<DiscoveredAssumption> {
    use crate::adapter::btor2::ast::Node;
    use crate::adapter::btor2::dep_graph::cone_leaf_nids;
    use crate::adapter::btor2::pin::pin_inputs_to_constants;
    use crate::adapter::btor2::symbolic_bitblast::exact_two_player_reach_realizable;
    const MAX_INPUT_BITS: u32 = 3;
    const MAX_CONJ_BITS: u32 = 12; // ≤ 4096 combinations searched
    let budget_ms: u128 = std::env::var("MUNUNU_ASSUMPTION_BUDGET_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(90_000);
    let start = std::time::Instant::now();

    let (register, value) = match parse_predicate_expr(good).ok() {
        Some(PredicateExpr::Cmp {
            register,
            op: CmpOp::Eq,
            value,
        }) => (register, value),
        _ => return None,
    };
    // Only when the game is unrealizable (a realizable game needs no assumption). Formula verdict, so
    // `good` may be a combinational-output / relational target (a FIFO's `full_o`).
    if exact_two_player_reach_realizable(btor2_content, good, controllable) != Ok(false) {
        return None;
    }
    let ctrl_set: std::collections::HashSet<&str> = controllable.iter().copied().collect();
    let Ok(file) = crate::adapter::btor2::parser::parse(btor2_content) else {
        return None;
    };
    let symbols = crate::adapter::btor2::parser::collect_symbols(&file);
    // Candidate axes = the narrow NON-controllable primary inputs IN `good`'s cone (dangling clocks are
    // out of the cone, so they never enter the combinatorial product).
    let cone = cone_leaf_nids(&file, std::slice::from_ref(&register));
    let mut axes: Vec<(String, u32)> = Vec::new();
    for line in &file.lines {
        if let Node::Input { sort, .. } = &line.node
            && cone.contains(&line.nid)
            && let Some(name) = symbols.get(&line.nid)
            && !ctrl_set.contains(name.as_str())
            && let Some(w) = crate::adapter::btor2::parser::bv_width(&file, *sort)
            && w <= MAX_INPUT_BITS
        {
            axes.push((name.clone(), w));
        }
    }
    axes.sort();
    axes.dedup();
    let total_bits: u32 = axes.iter().map(|(_, w)| *w).sum();
    if axes.len() < 2 || total_bits > MAX_CONJ_BITS {
        return None; // < 2 axes = not a conjunction; too wide = abstain (needs the symbolic ∃-env)
    }

    // The reused realizable + non-vacuous check on a pin set.
    let wins = |pins: &[(String, u64)]| -> bool {
        let (pinned, _) = pin_inputs_to_constants(btor2_content, pins);
        exact_two_player_reach_realizable(&pinned, good, controllable) == Ok(true)
            && good_nonvacuous_under(&pinned, &register, value)
    };

    // Search the product of the axes' value ranges for a WINNING full pin.
    let ranges: Vec<u64> = axes.iter().map(|(_, w)| 1u64 << w).collect();
    let total_combos: u64 = ranges.iter().product();
    let mut vals: Option<Vec<u64>> = None;
    for idx in 0..total_combos {
        if start.elapsed().as_millis() > budget_ms {
            break;
        }
        let mut rem = idx;
        let candidate: Vec<u64> = ranges
            .iter()
            .map(|r| {
                let v = rem % r;
                rem /= r;
                v
            })
            .collect();
        let pins: Vec<(String, u64)> = axes
            .iter()
            .zip(&candidate)
            .map(|((n, _), v)| (n.clone(), *v))
            .collect();
        if wins(&pins) {
            vals = Some(candidate);
            break;
        }
    }
    let vals = vals?;

    // MINIMIZE: greedily drop any pin the controller wins WITHOUT (that env input can stay free/adversarial).
    let mut needed: Vec<usize> = (0..axes.len()).collect();
    for i in 0..axes.len() {
        let trial: Vec<usize> = needed.iter().copied().filter(|&j| j != i).collect();
        let pins: Vec<(String, u64)> = trial
            .iter()
            .map(|&j| (axes[j].0.clone(), vals[j]))
            .collect();
        if wins(&pins) {
            needed = trial; // input i's pin is redundant
        }
    }
    if needed.len() < 2 {
        return None; // reduced to a single hold — the single-hold slice's job, not a conjunction
    }
    let phi = needed
        .iter()
        .map(|&j| format!("{} == {}", axes[j].0, vals[j]))
        .collect::<Vec<_>>()
        .join(" && ");
    Some(DiscoveredAssumption {
        phi,
        kind: AssumptionKind::InputConjunction,
        non_vacuous: true,
        engine: "two-player game realizable under env-input conjunction (pin-all + minimize)"
            .to_string(),
    })
}

/// Capability A (refined-verdicts Phase 1) — partition the `AG EF good` verdict over the CONFIG the
/// recovery rides on. For each config valuation (the cross-product of each `(input, candidate values)`
/// spec) PIN those inputs to constants and decide the CONCRETE pinned model with the exact engine —
/// which is 2-valued SOUND per cell (a pinned config is a constant, so the νµ is over a concrete
/// design). Returns the [`ConfigPartition`] the caller asked for — every enumerated cell, including a
/// ONE-cell partition (mununu#634: `--config-values rst_n=1` is a scoped verdict, "under this pin the
/// property is violated", and it is what `--discover-assumptions` keys off; collapsing it to `None`
/// hid exactly the cell the user named). Whether the property genuinely DEPENDS on config is
/// [`depends_on_config`]; the AUTO reset axis ([`auto_config_partition`]) applies it, because there
/// the user named nothing and a config-independent partition would only repeat the bare verdict.
/// `None` when `config_specs` is empty or the cross-product exceeds the enumeration cap (a wide/free
/// config needs the symbolic `∃config`, deferred).
///
/// **Engine (§16):** `exact-symbolic` full-state ROBDD (OxiDD) over each `pin_inputs_to_constants`
/// concrete model — no external tool, sidecar-free. The partition is sound OVER THE ENUMERATED config
/// values; `exhaustive` is set only when a spec lists a config input's FULL value range.
pub fn config_partition(
    btor2_content: &str,
    good: &str,
    config_specs: &[(String, Vec<u64>)],
) -> Option<ConfigPartition> {
    use crate::adapter::btor2::pin::pin_inputs_to_constants;
    // Cap the enumeration: 2^Σw valuations must stay tractable (else it is the wide/free-config case
    // that needs the symbolic `∃config` — deferred). Guard the multiply against overflow.
    const MAX_VALUATIONS: usize = 256;
    if config_specs.is_empty() {
        return None;
    }
    let total = config_specs
        .iter()
        .try_fold(1usize, |acc, (_, v)| acc.checked_mul(v.len().max(1)))?;
    if total == 0 || total > MAX_VALUATIONS {
        return None;
    }

    let widths = config_atom_widths(btor2_content, config_specs);
    let mut part = ConfigPartition {
        config_atoms: config_specs
            .iter()
            .map(|(n, _)| (n.clone(), *widths.get(n).unwrap_or(&0)))
            .collect(),
        engine: "exact-symbolic per-config pin".to_string(),
        // Exhaustive iff every spec enumerates its config input's FULL value range (all 2^w values):
        // a free config input reaches all its values, so the partition then covers every reachable
        // config. A user-supplied partial list ⇒ the partition is over the enumerated subset only.
        exhaustive: config_specs.iter().all(|(n, v)| {
            widths
                .get(n)
                .is_some_and(|&w| w < 63 && v.len() as u64 == 1u64 << w)
        }),
        ..Default::default()
    };

    for valuation in config_cross_product(config_specs) {
        let (pinned, _) = pin_inputs_to_constants(btor2_content, &valuation);
        match verify_recoverability_with_predicates(&pinned, good, &[])
            .unwrap_or(PropertyVerdict::Unknown)
        {
            PropertyVerdict::Holds => part.holds.push(valuation),
            PropertyVerdict::Violated => part.violated.push(valuation),
            PropertyVerdict::Unknown | PropertyVerdict::Skipped => part.unknown.push(valuation),
        }
    }

    Some(part)
}

/// Does the verdict genuinely DEPEND on the config — ≥2 non-empty verdict cells? A single non-empty
/// cell (all-holds / all-violated) is config-INDEPENDENT: the bare verdict already says it. The AUTO
/// axis reports only a dependent partition; an EXPLICIT spec is reported as asked (mununu#634).
fn depends_on_config(part: &ConfigPartition) -> bool {
    [&part.holds, &part.violated, &part.unknown, &part.vacuous]
        .iter()
        .filter(|c| !c.is_empty())
        .count()
        >= 2
}

/// Capability A, slice 2b — AUTO config-atom identification. When the caller does not name config
/// inputs (bare `--refine`), the design's DETECTED RESET is the clearest, lowest-noise config axis:
/// it is 1-bit (trivially enumerable), structurally identifiable (the reset mux — [`detect_resets`]),
/// and drives every state cell — so partitioning `AG EF(good)` over {reset-held, operational} surfaces
/// the branching pair "recoverable held-in-reset / trapped operationally" without the user naming
/// anything (the OpenTitan `aes_cipher` / `csrng` case). Returns `None` when no reset is detected, when
/// there are implausibly many resets to enumerate, or when the property is reset-INDEPENDENT
/// ([`config_partition`] already collapses that to `None`).
///
/// Wider / free config atoms (a ≥8-bit mode datapath, an arbitrary config register) exceed the
/// enumeration cap and need the symbolic `∃config` path — deferred; only the reset axis is
/// auto-enumerated here.
///
/// **Engine (§16):** `exact-symbolic` per-reset-value pin (same as [`config_partition`]); the reset is
/// discovered by the structural [`crate::adapter::fsm_scan::detect_resets`] reset-mux scan — no tool,
/// sidecar-free.
fn auto_config_partition(btor2_content: &str, good: &str) -> Option<ConfigPartition> {
    let file = crate::adapter::btor2::parser::parse(btor2_content).ok()?;
    let symbols = crate::adapter::btor2::parser::collect_symbols(&file);
    let resets = crate::adapter::fsm_scan::detect_resets(&file, &symbols);
    // 1-bit resets: 2^n valuations. Keep well under the 256 cap and skip a design with an implausible
    // number of distinct reset inputs (not the single-reset axis this targets).
    if resets.is_empty() || resets.len() > 4 {
        return None;
    }
    // Partition over BOTH reset levels {0,1} (`detect_resets` yields only the inactive level; the
    // property may hold at one and be violated/vacuous at the other — that IS the branching pair to
    // surface). `config_partition` reports only when the cells genuinely differ.
    let specs: Vec<(String, Vec<u64>)> = resets.into_iter().map(|(n, _)| (n, vec![0, 1])).collect();
    config_partition(btor2_content, good, &specs).filter(depends_on_config)
}

/// The cross-product of the per-input candidate value lists → every config valuation.
fn config_cross_product(specs: &[(String, Vec<u64>)]) -> Vec<Vec<(String, u64)>> {
    let mut out: Vec<Vec<(String, u64)>> = vec![Vec::new()];
    for (name, values) in specs {
        let mut next = Vec::with_capacity(out.len() * values.len().max(1));
        for partial in &out {
            for &v in values {
                let mut ext = partial.clone();
                ext.push((name.clone(), v));
                next.push(ext);
            }
        }
        out = next;
    }
    out
}

/// Best-effort `(name → bit-width)` for the config-spec inputs, resolved from the parsed design.
fn config_atom_widths(
    btor2_content: &str,
    specs: &[(String, Vec<u64>)],
) -> std::collections::HashMap<String, u32> {
    use crate::adapter::btor2::ast::Node;
    let mut widths = std::collections::HashMap::new();
    let Ok(file) = crate::adapter::btor2::parser::parse(btor2_content) else {
        return widths;
    };
    let symbols = crate::adapter::btor2::parser::collect_symbols(&file);
    for (name, _) in specs {
        if let Some(nid) = symbols.iter().find_map(|(n, s)| (s == name).then_some(*n))
            && let Some(sort) = match file.lookup(nid).map(|l| &l.node) {
                Some(Node::Input { sort, .. }) | Some(Node::State { sort, .. }) => Some(*sort),
                _ => None,
            }
            && let Some(w) = crate::adapter::btor2::parser::bv_width(&file, sort)
        {
            widths.insert(name.clone(), w);
        }
    }
    widths
}

/// Harness / attribution seam — run ONLY the SPCR pre-pass ([`array_prophecy::spcr`]), then the
/// exact ROBDD on the array-free result. Isolates SPCR's contribution, which is otherwise embedded
/// inside [`verify_recoverability_scalable`] (so a `dflt`/`wp` HOLDS on an array-content case cannot
/// be attributed to SPCR specifically). Returns:
/// - `Holds` / `Violated` — SPCR applied (array-content shape registerized, array dropped) and the
///   exact engine decided the array-free design (a verdict-preserving reformulation — transfers).
/// - `Unknown` — SPCR applied but the array-free cone is still too wide for exact (would fall to the
///   cube in the production path).
/// - `Skipped` — SPCR did NOT apply (no in-cone array, or an unsound / outside-the-fragment shape:
///   a moving index selected by a free input independent of the write address, a sweeping index,
///   etc. → SPCR soundly abstains).
///
/// Used by the wall-class matrix's `spcr` column to MEASURE SPCR's marginal reach (it decides the
/// array-content-gated νµ class that exact-alone SKIPs and the plain ∀-array cube leaves ⊥) and to
/// mark the fragment boundary (the independent-index sibling → `Skipped`).
///
/// **Engine (§16):** `exact-symbolic` ROBDD (OxiDD) over the SPCR-produced ARRAY-FREE full-state
/// design — the array-content atom is now a plain `pv == K` BV register, so the µ-fixpoint
/// bit-blasts. `Skipped` when the SPCR reformulation does not apply.
pub fn verify_recoverability_spcr_only(
    btor2_content: &str,
    good: &str,
) -> Result<PropertyVerdict, String> {
    parse_predicate_expr(good).map_err(|e| {
        format!("recoverability target `{good}` is not a register-comparison atom (`REG op VALUE`): {e:?}")
    })?;
    let file0 = crate::adapter::btor2::parser::parse(btor2_content)
        .map_err(|e| format!("SPCR-only: parsing BTOR2: {}", e.message))?;
    match crate::adapter::btor2::array_prophecy::spcr(&file0) {
        Some(af) => {
            let src = crate::adapter::btor2::emit::emit_btor2(&af);
            let formula_str = format!("nu Y. ((mu X. (({good}) || <> X)) && [] Y)");
            let formula = mu_parser::parse(&formula_str).map_err(|e| {
                format!("SPCR-only: building the AG EF formula for `{good}`: {e:?}")
            })?;
            match crate::adapter::btor2::symbolic_bitblast::exact_symbolic_verdict(&src, &formula) {
                Ok(v) => Ok(PropertyVerdict::from(v)),
                // SPCR applied (array dropped) but the array-free cone is still too wide for exact.
                Err(_) => Ok(PropertyVerdict::Unknown),
            }
        }
        // SPCR does not apply (no array / outside the decidable fragment) → sound abstention.
        None => Ok(PropertyVerdict::Skipped),
    }
}

/// b2 — decide `AG EF good` by COMPOSING a ranking certificate (every in-cone
/// down-counter that gates progress always eventually expires) with b1's counter
/// may-abstraction ([`counter_may_abstract`](crate::adapter::btor2::bit_blast::counter_may_abstract)),
/// then the EXACT engine on the resulting SMALL model. This is the νμ lever for an
/// FSM-gated-by-counter recoverability target (e.g. i2c's SCL recurrence) that the
/// bare cube leaves `⊥` because the wide counter blows its state space and the
/// may-abstracted decrement carries no must-progress.
///
/// **Soundness.** b1 replaces each certified counter with a 1-bit `cnt == T` register
/// whose decrement is a free (may) input — its ONLY added behaviour.
/// [`ranking_certificate_holds`] certifies each such counter descends to its threshold
/// from every reachable state, so the abstract "counter expires" step is a genuine
/// eventuality; and because the gated advance is conditioned on `cnt == T`, the FSM is
/// held while `cnt ≠ T`, so the abstract single-step expiry and the concrete K-step
/// descent reach the SAME control state. Hence every abstract `EF good` witness lifts to
/// a concrete path, and the over-approximating outer `AG` only strengthens a `Holds`
/// claim. We therefore trust ONLY a `Holds` verdict on the abstract model; a `Violated`
/// there could be a spurious over-approximation and is mapped to abstain (`None`).
/// `good_registers` are excluded from abstraction so the property atom never desyncs.
///
/// **Detection scope.** [`detect_down_counter`](crate::adapter::btor2::bit_blast::detect_down_counter)
/// recognises a counter whose stride operates on the raw state nid OR on a reset-mux alias
/// `ite(reset, cnt, const)` (the yosys async-reset lowering — `sub(ite(reset,cnt,0),1)`,
/// `redor(ite(reset,cnt,0))`), and ignores dead observable copies (`uext(cnt,0)` named
/// `…cnt`). So both hand-authored BTOR2 and yosys-lifted RTL are covered. The residual
/// limit is the **fairness wall**: a counter whose reload is gated by a demonic input
/// (an `ena`/sync an adversary can hold to stall the descent) has no ranking certificate —
/// `AG EF good` there genuinely needs a fairness assumption mununu cannot make, so that
/// counter is not abstracted and the property falls to the cube (the sound answer). (i2c's
/// SCL recurrence is exactly this: `dcnt` certifies but the `ena`-gated clock divider `cnt`
/// does not, leaving the cone too wide → ⊥.)
fn verify_recoverability_counter_abstracted(
    file: &crate::adapter::btor2::ast::Btor2File,
    good: &str,
    good_registers: &[String],
) -> Option<PropertyVerdict> {
    use crate::adapter::btor2::bit_blast::{counter_may_abstract, detect_down_counter};
    let symbols = crate::adapter::btor2::parser::collect_symbols(file);
    // Gating down-counters the good atom does NOT read, each with a ranking-certified
    // descent to its threshold (so its abstract may-expiry is a real eventuality).
    let counters: Vec<_> = detect_down_counter(file)
        .into_iter()
        .filter(|c| {
            symbols
                .get(&c.nid)
                .is_none_or(|s| !good_registers.contains(s))
        })
        .filter(|c| {
            symbols.get(&c.nid).is_some_and(|s| {
                ranking_certificate_holds(file, std::slice::from_ref(s), Some(c.threshold))
            })
        })
        .collect();
    if counters.is_empty() {
        return None;
    }
    // Apply b1 to every certified counter (disjoint registers, NIDs preserved).
    let mut abstracted = file.clone();
    for c in &counters {
        abstracted = counter_may_abstract(&abstracted, c)?;
    }
    let abstract_src = crate::adapter::btor2::emit::emit_btor2(&abstracted);
    let formula_str = format!("nu Y. ((mu X. (({good}) || <> X)) && [] Y)");
    let formula = mu_parser::parse(&formula_str).ok()?;
    match crate::adapter::btor2::symbolic_bitblast::exact_symbolic_verdict(&abstract_src, &formula)
    {
        Ok(crate::adapter::btor2::symbolic_bitblast::ExactVerdict::Holds) => {
            Some(PropertyVerdict::Holds)
        }
        _ => None,
    }
}

/// A best-effort, SOUND-hint explanation of WHY an `AG EF good` recoverability property stayed ⊥ —
/// so a ⊥ becomes ACTIONABLE ("what would it take to decide this?") instead of a bare "don't know".
/// Purely a diagnostic: it NEVER produces or changes a verdict. Localizes the structural obstacles in
/// the good's recovery cone that the predicate abstraction cannot cross.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub struct RecoverabilityBotDiagnosis {
    /// Down-counters that GATE the recovery (in the good's cone, not read by the good) whose descent
    /// is NOT ranking-certified — a fairness-gated reload or a non-affine descent: `(name, width)`.
    /// Deciding needs a ranking certificate or a fairness assumption.
    pub uncertified_counters: Vec<(String, u32)>,
    /// Wide state / inputs (NOT a small FSM, NOT a certified counter) whose value flows into the
    /// recovery target: `(name, width)`. The recovery value rides that wide state, so the verdict is
    /// data/config-scoped (a specific config may HOLD, another VIOLATE) — the predicate cube cannot
    /// enumerate it. Constrain those inputs, or the ⊥ is the honest config-independent answer.
    pub wide_influences: Vec<(String, u32)>,
}

impl RecoverabilityBotDiagnosis {
    /// True when no structural obstacle was localized — the abstraction may simply be too coarse
    /// (more predicates could close it).
    pub fn is_empty(&self) -> bool {
        self.uncertified_counters.is_empty() && self.wide_influences.is_empty()
    }

    /// A one-line, actionable hint for the ⊥ note.
    pub fn hint(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.uncertified_counters.is_empty() {
            let names: Vec<String> = self
                .uncertified_counters
                .iter()
                .map(|(n, w)| format!("`{n}` ({w}-bit)"))
                .collect();
            parts.push(format!(
                "gated by {} counter(s) with an un-certified descent [{}] — needs a ranking \
                 certificate or a fairness assumption",
                self.uncertified_counters.len(),
                names.join(", ")
            ));
        }
        if !self.wide_influences.is_empty() {
            let names: Vec<String> = self
                .wide_influences
                .iter()
                .map(|(n, w)| format!("`{n}` ({w}-bit)"))
                .collect();
            parts.push(format!(
                "the recovery value rides wide data/config state [{}] the predicate cube cannot \
                 enumerate — the verdict is input-scoped (constrain those inputs to decide, else ⊥ \
                 is the honest config-independent answer)",
                names.join(", ")
            ));
        }
        if parts.is_empty() {
            "no structural obstacle localized by the best-effort cone walk — the ⊥ may be a coarse \
             but closable abstraction (add predicates), OR a deeper config/sequence dependence this \
             light pass does not localize"
                .to_string()
        } else {
            parts.join("; ")
        }
    }
}

/// See [`RecoverabilityBotDiagnosis`]. `good` is the `REG == VALUE` recovery atom; a parse failure
/// yields an empty diagnosis (a hint is never load-bearing).
pub fn diagnose_recoverability_bot(btor2_content: &str, good: &str) -> RecoverabilityBotDiagnosis {
    use crate::adapter::btor2::ast::Node;
    use crate::adapter::btor2::parser::{collect_symbols, find_next_value_operand};
    const WIDE_MIN_WIDTH: u32 = 8;
    const MAX_LISTED: usize = 4;
    let mut diag = RecoverabilityBotDiagnosis::default();
    let Ok(file) = crate::adapter::btor2::parser::parse(btor2_content) else {
        return diag;
    };
    let Ok(good_expr) = parse_predicate_expr(good) else {
        return diag;
    };
    let good_registers = good_expr.registers();
    let symbols = collect_symbols(&file);

    // The good's BACKWARD cone (crossing state boundaries): the named cells/inputs whose value
    // transitively determines the recovery target. Resolve each good register to a seed nid — an
    // Input/State/Op-alias (via `symbols`) OR a top-level `output` node carrying the atom. A lifted
    // design's recovery target is commonly an `output` (e.g. i2c's `scl_padoen_o`), which
    // `collect_symbols` does NOT index — without this fallback the cone seeds empty and the diagnosis
    // reports "no obstacle" on exactly the lifted designs it serves. Seed from the output nid; the
    // walk below follows `Node::Output.signal` into the driving logic.
    let resolve_seed = |gr: &String| -> Option<i64> {
        symbols
            .iter()
            .find_map(|(n, s)| (s == gr).then_some(*n))
            .or_else(|| {
                file.lines.iter().find_map(|l| match &l.node {
                    Node::Output {
                        symbol: Some(s), ..
                    } if s == gr => Some(l.nid),
                    _ => None,
                })
            })
    };
    let mut queue: std::collections::VecDeque<i64> =
        good_registers.iter().filter_map(resolve_seed).collect();
    let mut cone: std::collections::HashSet<i64> = std::collections::HashSet::new();
    while let Some(nid) = queue.pop_front() {
        if !cone.insert(nid) {
            continue;
        }
        match file.lookup(nid).map(|l| &l.node) {
            Some(Node::State { .. }) => {
                if let Some(nv) = find_next_value_operand(&file, nid) {
                    queue.push_back(nv.0.abs());
                }
            }
            Some(Node::Op { args, .. }) => {
                for o in args {
                    queue.push_back(o.0.abs());
                }
            }
            // A combinational OUTPUT (e.g. `cs_n`) carries the atom on an `output`/alias node —
            // follow its driving signal so the walk reaches the underlying state + datapath.
            Some(Node::Output { signal, .. }) => {
                queue.push_back(signal.0.abs());
            }
            _ => {}
        }
    }

    // (1) Uncertified gating down-counters in the cone (the good does not read them). A yosys-lifted
    // INTERNAL counter is UNNAMED; the name-based ranking pipeline cannot reference it, so it is
    // uncertified in practice — report it by a synthesized `nid<N>(<W>-bit)` label rather than
    // silently dropping it. (Pre-fix the diagnosis was symbol-gated: it skipped every unnamed
    // obstacle and reported "no structural obstacle" on exactly the real lifted designs it serves —
    // e.g. i2c's counter-gated ⊥, whose 16-bit SCL prescaler is an unnamed internal register.)
    let counter_nids: std::collections::HashSet<i64> =
        crate::adapter::btor2::bit_blast::detect_down_counter(&file)
            .iter()
            .map(|c| c.nid)
            .collect();
    for c in crate::adapter::btor2::bit_blast::detect_down_counter(&file) {
        if !cone.contains(&c.nid) {
            continue;
        }
        match symbols.get(&c.nid) {
            // Named: skip if the good reads it, or its descent is ranking-certified (unchanged).
            Some(sym) => {
                if !good_registers.contains(sym)
                    && !ranking_certificate_holds(
                        &file,
                        std::slice::from_ref(sym),
                        Some(c.threshold),
                    )
                {
                    diag.uncertified_counters.push((sym.clone(), c.width));
                }
            }
            // Unnamed lifted counter: unreachable by the name-based ranking pipeline ⇒ uncertified
            // by construction — surface it by nid so the ⊥ hint is actionable on lifts.
            None => diag
                .uncertified_counters
                .push((format!("nid{}({}-bit)", c.nid, c.width), c.width)),
        }
    }

    // (2) Wide data/config state the recovery value rides — EXCLUDING small FSM states (enum-detected,
    // cube-handleable) and the certified/uncertified counters (already reported / handled).
    let enum_nids: std::collections::HashSet<i64> =
        crate::adapter::btor2::bit_blast::detect_enum_states(&file)
            .iter()
            .map(|e| e.nid)
            .collect();
    for nid in &cone {
        if counter_nids.contains(nid) || enum_nids.contains(nid) {
            continue;
        }
        // A NAMED cone register that is the good's own value is not an obstacle; an unnamed lifted
        // register has no symbol, so it can never be the (named) good register — do not skip it.
        if let Some(sym) = symbols.get(nid)
            && good_registers.contains(sym)
        {
            continue;
        }
        let sort = match file.lookup(*nid).map(|l| &l.node) {
            Some(Node::State { sort, .. }) | Some(Node::Input { sort, .. }) => *sort,
            _ => continue,
        };
        if let Some(w) = crate::adapter::btor2::parser::bv_width(&file, sort)
            && w >= WIDE_MIN_WIDTH
        {
            // Named: report the symbol; unnamed lifted state: report by nid (the symbol-gate fix).
            let label = symbols
                .get(nid)
                .cloned()
                .unwrap_or_else(|| format!("nid{nid}({w}-bit)"));
            diag.wide_influences.push((label, w));
        }
    }

    diag.uncertified_counters.sort();
    diag.uncertified_counters.dedup();
    diag.uncertified_counters.truncate(MAX_LISTED);
    diag.wide_influences.sort();
    diag.wide_influences.dedup();
    diag.wide_influences.truncate(MAX_LISTED);
    diag
}

/// P2 Slice 1 — decide recoverability `AG EF good` via the **predicate-cube +
/// `smt-hyper-must`** path, so the νμ property decides beyond the exact engine's
/// ~40-bit cone cap.
///
/// `good` is either a simple `REG == VALUE` equality atom OR a relational `REG == REG` atom (a
/// relational recoverability target — e.g. a FIFO's `AG EF (wptr == rptr)`, decided by the compound-good
/// machinery); any other comparison (`<`, `>=`, an addend form, …) returns `Ok(Unknown)` (an honest
/// abstain — the reset-cube construction is well-defined only for `==`). `extra_predicates` are
/// additional abstraction predicates that refine the cube (they do not enter the formula). Returns the
/// canonical verdict at the design's reset cube, or `Ok(Unknown)` when the abstraction cannot decide or
/// the CEGAR loop errors (a sound abstain — never a fabricated definite verdict). NOTE: a relation
/// reached by unbounded progress (a FIFO draining `wptr` up to `rptr` — independent counters) is the
/// ranking class and soundly abstains at scale; only invariant / bounded-must relations decide.
pub fn verify_recoverability_scalable(
    btor2_content: &str,
    good: &str,
    extra_predicates: &[PredicateSpec],
) -> Result<PropertyVerdict, String> {
    verify_recoverability_scalable_with_source(
        btor2_content,
        good,
        extra_predicates,
        default_craig_source(),
    )
}

/// The default CEGAR predicate source: the `MUNUNU_CRAIG_REFINE` opt-in (WP otherwise). Used by
/// every non-planner caller (CLI / API / tests) so their behaviour is unchanged.
pub fn default_craig_source() -> PredicateSource {
    if std::env::var_os("MUNUNU_CRAIG_REFINE").is_some() {
        PredicateSource::CraigInterpolation
    } else {
        PredicateSource::WeakestPrecondition
    }
}

/// [`verify_recoverability_scalable`] with the CEGAR predicate source chosen by the CALLER (the
/// planner / a validation harness) rather than the `MUNUNU_CRAIG_REFINE` env — front-B's
/// integration seam. The planner schedules [`PredicateSource::CraigInterpolation`] when its
/// MathSAT dependency is available; the wall-class matrix uses it to run the Craig lever
/// deterministically (no racy global env). Craig always falls back to WP inside the loop, so an
/// explicit `Craig` source can only DECIDE MORE (sound + monotone under `SmtHyperMust`).
pub fn verify_recoverability_scalable_with_source(
    btor2_content: &str,
    good: &str,
    extra_predicates: &[PredicateSpec],
    predicate_source: PredicateSource,
) -> Result<PropertyVerdict, String> {
    // Parse `good` and require a `REG == VALUE` equality atom.
    let good_expr = parse_predicate_expr(good).map_err(|e| {
        format!("recoverability target `{good}` is not a register-comparison atom (`REG op VALUE`): {e:?}")
    })?;
    // The good atom may be a simple `REG == VALUE` OR a relational `REG == REG`. The latter enables
    // relational recoverability targets — e.g. a FIFO's `AG EF (wptr == rptr)` ("always able to drain to
    // empty") — decided by the same relational compound-predicate machinery as frontier (b). Any other
    // comparison (`<`, `>=`, an addend form, …) is an honest abstain: the reset-cube construction below
    // is only well-defined for `==`.
    let (good_registers, good_simple): (Vec<String>, Option<(String, u64)>) = match good_expr {
        PredicateExpr::Cmp {
            register,
            op: CmpOp::Eq,
            value,
        } => (vec![register.clone()], Some((register, value))),
        PredicateExpr::CmpReg {
            ref lhs,
            op: CmpOp::Eq,
            ref rhs,
        } => (vec![lhs.clone(), rhs.clone()], None),
        _ => return Ok(PropertyVerdict::Unknown),
    };

    // Parse the design early — needed both for the reset valuation and for the auto-seed candidate
    // values below.
    let file0 = crate::adapter::btor2::parser::parse(btor2_content)
        .map_err(|e| format!("recoverability cube path: parsing BTOR2: {}", e.message))?;

    // SPCR (shot ①b) — Selective Prophecy-Cell Registerization. If every in-cone array matches the
    // sound P-A1 shape (single unconditional full-width write, read at stable/move-to-write indices),
    // eliminate the arrays by registerizing exactly the accessed cells (a prophecy register + exact
    // frame). The result is array-FREE, so the must-edge ∀∃ becomes pure QF_BV (decidable) instead of
    // the undecidable AUFBV+∀-array that leaves the νµ at ⊥. Verdict-preserving reformulation, so a
    // definite exact verdict on the array-free design transfers. `None` ⇒ no array / not the sound
    // shape ⇒ keep the original (honest ⊥ downstream). See `array_prophecy` + the plan doc.
    let spcr_src;
    let (file, btor2_content): (crate::adapter::btor2::ast::Btor2File, &str) =
        match crate::adapter::btor2::array_prophecy::spcr(&file0) {
            Some(af) => {
                spcr_src = crate::adapter::btor2::emit::emit_btor2(&af);
                // The array-free cone often now bit-blasts — retry the EXACT ROBDD first (a small
                // array-free design decides definitively; the array-gated recovery the wall blocked).
                let formula_str = format!("nu Y. ((mu X. (({good}) || <> X)) && [] Y)");
                if let Ok(formula) = mu_parser::parse(&formula_str)
                    && let Ok(v) = crate::adapter::btor2::symbolic_bitblast::exact_symbolic_verdict(
                        &spcr_src, &formula,
                    )
                {
                    return Ok(PropertyVerdict::from(v));
                }
                // Cone still too wide for exact even array-free → fall through to the cube on the
                // array-free design (the guard `pv == K` is now a plain BV predicate; QF_BV must).
                (af, spcr_src.as_str())
            }
            None => (file0, btor2_content),
        };
    let init_values: BTreeMap<String, u128> =
        crate::adapter::btor2::concrete_oracle::init_valuation(&file);

    // N1 RANKING RESCUE: `AG EF good` may HOLD by a well-founded datapath descent (a down-counter to 0,
    // a timer, a deterministic drain) that the predicate cube cannot capture — the RANKING class. Prove
    // it directly over the exact transition BEFORE the cube; a sound sufficient condition (it establishes
    // the stronger `AG AF good`). If it fails, fall through to the cube (the certificate is sufficient,
    // not necessary).
    if ranking_certificate_holds(
        &file,
        &good_registers,
        good_simple.as_ref().map(|(_, v)| *v),
    ) {
        return Ok(PropertyVerdict::Holds);
    }

    // N1 BOOLEAN-GATED-EVENT RANKING RESCUE: a `good` EVENT like `timeout == 1` / `done == 1` carries
    // no descending measure itself, but if it is driven EXACTLY by a `counter == threshold` gate, the
    // event recoverability `AG EF (sig == 1)` reduces to the counter recoverability `AG EF (counter ==
    // threshold)` — which the ranking DOES decide (the counter descends/ascends to the threshold).
    // Sound: `counter_gate_of` only rewrites a PURE `Eq` driver (so `sig == 1 ⟺ counter == threshold`),
    // and a ranking Holds on the counter transfers to the event (`EF` absorbs the one-cycle register
    // delay). A reload/kick that stalls the descent leaves the ranking un-certified ⇒ fall to the cube.
    if let Some((sig, 1)) = good_simple.as_ref().map(|(s, v)| (s.clone(), *v))
        && let Some((counter, threshold)) = counter_gate_of(&file, &sig)
        && ranking_certificate_holds(&file, &[counter], Some(threshold))
    {
        return Ok(PropertyVerdict::Holds);
    }

    // b2 COUNTER-ABSTRACTION RESCUE: when `good` is gated (through an FSM) by one or more
    // in-cone DOWN-COUNTERS whose descent is ranking-certified, collapse each to its
    // `{cnt==T}` bit (b1) and decide `AG EF good` with the EXACT engine on the resulting
    // small model. Handles the FSM-gated-by-counter shape (i2c's SCL recurrence) that the
    // direct counter-gate rescue above and the bare cube both miss. Sound: only a `Holds`
    // is trusted (see `verify_recoverability_counter_abstracted`); a non-Holds falls through
    // to the cube, so this is never less sound than the prior behaviour.
    if let Some(v) = verify_recoverability_counter_abstracted(&file, good, &good_registers) {
        return Ok(v);
    }

    // Seed predicates = [good] ++ [good register's OTHER control states] ++ extra. The `good`
    // predicate is named `good` so the formula atom resolves to it via the cube labelling; the
    // rest only refine the abstraction (they are not referenced by the formula).
    let mut specs: Vec<PredicateSpec> = Vec::with_capacity(1 + extra_predicates.len());
    // The `good` predicate (named `good` so the formula atom resolves to it). A SIMPLE target is a cube
    // dimension `register == value`; a RELATIONAL target is a COMPOUND predicate carried in
    // `good_compound` and added to `compound_seeds` below, so the reset cube evaluates the relation
    // (`lhs == rhs`) rather than a `register == value` atom.
    let good_compound: Option<(String, String, PredicateExpr)> = match &good_simple {
        Some((register, value)) => {
            specs.push(PredicateSpec {
                name: "good".to_string(),
                register: register.clone(),
                value: *value,
            });
            None
        }
        None => {
            let expr_str = format!("{} == {}", good_registers[0], good_registers[1]);
            let expr = parse_predicate_expr(&expr_str)
                .map_err(|e| format!("relational recoverability good `{expr_str}`: {e:?}"))?;
            Some(("good".to_string(), expr_str, expr))
        }
    };

    // N1 first increment — property-directed discovery (2026-07-11). Recoverability `EF good` must
    // DISTINGUISH the control states on the return path, not just `good` vs `!good`: a `good`-only
    // abstraction lumps every non-good control state into one cube whose must-successor self-loops,
    // so `EF good` is `⊥` (e.g. RESPONDER's `{st!=0}` lumps `req`/`grant` — it needs `st==1` and
    // `st==2`). Auto-seed `good_register == v` for each candidate value `v != good_value`, where the
    // candidate pool is `{0,1} ∪ the design's constant literals` (the FSM state encodings). This is
    // BOUNDED — `O(#constants)`, NOT `2^W` — so a wide `good` register does not blow up; capped for
    // safety. Directed by the `good` atom (the property), it recovers the control-return
    // recoverability class the coarse abstraction abstained on. Sound: adding predicates only
    // sharpens `⊥` toward a definite verdict (monotone refinement), never flips one.
    // Frontier (a): the constant pool is the good register's OWN cone constants (property-directed),
    // not every global constant — a design with many unrelated constants would otherwise seed
    // `good == decoy` predicates the good register can never take, inflating the cube. Fall back to the
    // global pool only when the cone is empty (good register absent / no `next`).
    const MAX_AUTO_SEED: usize = 8;
    for gr in &good_registers {
        let cone_constants = good_next_cone_constants(&file, gr);
        let const_pool = if cone_constants.is_empty() {
            crate::adapter::btor2::bit_blast::collect_btor2_constants(&file)
        } else {
            cone_constants
        };
        // A constant the register cannot hold is not a discriminator: `ctrl == 2` on a 1-bit
        // `ctrl` is an always-false atom (the cone's constants include the datapath's — the
        // 48-bit addend `2` of `data == target + 2` reached a 1-bit control register this way).
        // Under the SMT lowering's former masking it was worse than useless: it aliased
        // `ctrl == 0` and the lift carried a second spelling of the same atom.
        let fits = |v: u64| match register_width(&file, gr) {
            Some(w) if w < 64 => v < (1u64 << w),
            _ => true,
        };
        let mut candidate_values: Vec<u64> = vec![0, 1];
        for v in const_pool {
            if fits(v) && !candidate_values.contains(&v) {
                candidate_values.push(v);
            }
        }
        for v in candidate_values {
            // Cap the total dimension count at good + MAX_AUTO_SEED to bound the cube space.
            if specs.len() > MAX_AUTO_SEED {
                break;
            }
            // Skip the good value only for the SIMPLE good's OWN register (that value IS the good atom;
            // for a relational good every value is a legitimate discriminator).
            let is_good_val = good_simple
                .as_ref()
                .is_some_and(|(r, val)| r == gr && *val == v);
            if !is_good_val && !specs.iter().any(|s| s.register == *gr && s.value == v) {
                specs.push(PredicateSpec {
                    name: format!("state_{gr}_eq_{v}"),
                    register: gr.clone(),
                    value: v,
                });
            }
        }
    }

    // Class-2 (datapath-DEPENDENT return): seed the design's `register == constant` GUARD atoms — the
    // decision conditions the control branches on (e.g. `busy → done` only when `data == K`). When
    // the must-return reads a datapath predicate, control-state seeding alone leaves `EF good` at `⊥`;
    // the needed predicate is `data == K`, whose value K CANNOT be enumerated (`2^W`), but IS a design
    // literal read by a comparison node. Extracting those comparison atoms is the property-directed
    // discovery of the datapath predicate from the design's own guards (the shipped, eager form of the
    // lazy "discover from the ⊥ obligation" idea). Bounded by the number of comparison nodes; deduped
    // against what is already seeded; capped with the control seeding at MAX_AUTO_SEED.
    // N1 frontier (a) — property-directed guard discovery: restrict the guard-atom seeds to the good
    // register's cone-of-influence. A guard over a register the return path does not depend on (a decoy
    // comparison on an unrelated register) cannot change the verdict but still doubles the cube; without
    // this filter a design with many unrelated guards blows up the `2^{|P|}` all-pairs SMT. An empty
    // cone (good register absent / no `next`) means "no restriction" — prior behaviour preserved.
    let good_coi: std::collections::HashSet<String> = good_registers
        .iter()
        .flat_map(|gr| good_register_coi(&file, gr))
        .collect();
    let in_coi = |reg: &str| good_coi.is_empty() || good_coi.contains(reg);

    for (reg, k) in eq_guard_atoms(&file) {
        if specs.len() > MAX_AUTO_SEED {
            break;
        }
        if in_coi(&reg) && !specs.iter().any(|s| s.register == reg && s.value == k) {
            specs.push(PredicateSpec {
                name: format!("guard_{reg}_eq_{k}"),
                register: reg,
                value: k,
            });
        }
    }

    specs.extend(extra_predicates.iter().cloned());

    // Class-2 RELATIONAL return: seed the design's `register == register` guard atoms — the relational
    // decision conditions the control branches on (`busy → done` gated on `data == target`). These are
    // COMPOUND predicates (two registers, no literal), so they flow through the sidecar's
    // `compound_predicates`: the SMT hyper-must seam decides each cube's truth via the EXACT transition
    // (preserving the relational invariant across the wide datapath without concretising it), and the
    // reset cube below evaluates the relational expr rather than `register == value`. Bounded by the
    // number of comparison nodes; deduped implicitly (a `data == target` seed re-derives to the same
    // expr); capped WITH the other auto-seeds so the cube dimension count stays `≤ MAX_AUTO_SEED + 1`.
    // Built via `parse_predicate_expr` on the same string the sidecar carries, so the local expr used
    // for the reset-cube eval is byte-identical to the one the CEGAR lift parses.
    let mut compound_seeds: Vec<(String, String, PredicateExpr)> = Vec::new();
    // A RELATIONAL good target is the first compound predicate (named `good`; the formula references it).
    if let Some(gc) = good_compound {
        compound_seeds.push(gc);
    }
    for (lhs, rhs) in eq_reg_guard_atoms(&file) {
        if specs.len() + compound_seeds.len() > MAX_AUTO_SEED {
            break;
        }
        // Frontier (a): both operands must be in the good register's cone (a relational guard over
        // unrelated registers cannot change the verdict).
        if !(in_coi(&lhs) && in_coi(&rhs)) {
            continue;
        }
        let expr_str = format!("{lhs} == {rhs}");
        let Ok(expr) = parse_predicate_expr(&expr_str) else {
            continue;
        };
        compound_seeds.push((format!("rel_{lhs}_eq_{rhs}"), expr_str, expr));
    }

    // Frontier (b′) — arithmetic-relational returns `data == target + K` (the addend form the bare
    // relational extraction misses). Same compound-predicate path; the LOCAL expr carries the register
    // width so the reset-cube eval does the mod-2^width arithmetic the design's `+` wraps by (the sidecar
    // string parses to a width-agnostic BV form for the SMT seam, which is width-implicit).
    for (lhs, rhs, addend, width) in eq_reg_addend_guard_atoms(&file) {
        if specs.len() + compound_seeds.len() > MAX_AUTO_SEED {
            break;
        }
        if !(in_coi(&lhs) && in_coi(&rhs)) {
            continue;
        }
        let name = format!("rel_{lhs}_eq_{rhs}_plus_{addend}");
        if compound_seeds.iter().any(|(n, _, _)| n == &name) {
            continue;
        }
        let expr_str = format!("{lhs} == {rhs} + {addend}");
        let expr = PredicateExpr::CmpRegAddend {
            lhs: lhs.clone(),
            op: CmpOp::Eq,
            rhs: rhs.clone(),
            addend,
            width,
        };
        compound_seeds.push((name, expr_str, expr));
    }

    // SEL (P1-a, shot ①b) — ARRAY-CONTENT return: seed the design's `array[index] == K` guard atoms
    // (`select_guard_atoms`) as COMPOUND `PredicateExpr::Select` predicates. When the must-return reads an
    // in-cone MEMORY (`busy → idle` only when `mem[key] == unlock`), control/register seeding leaves
    // `EF good` at ⊥ — the array merges into one cube cell — so the deciding predicate is `mem[key] == K`.
    // SMT-only (like the relational seeds): the `SmtHyperMust` seam decides each cube's truth via the exact
    // transition over Z3's array `select` (QF_AUFBV), never bit-blasting the array. Cone-filtered + capped.
    for (arr, idx, k) in select_guard_atoms(&file) {
        if specs.len() + compound_seeds.len() > MAX_AUTO_SEED {
            break;
        }
        // Property-directed filter keys on the ARRAY (a state cell in the good's cone-of-
        // influence, per `good_register_coi`'s state traversal): a `mem[·]` predicate is
        // relevant iff the array's content flows into recovery. The index is a combinational
        // reset-mux alias (`u.key`, not a state) so it is absent from the states-only cone
        // set — checking `in_coi(idx)` would spuriously drop every valid array-content seed.
        // The index only selects which cell; it needs no independent cone membership.
        if !in_coi(&arr) {
            continue;
        }
        let name = format!("sel_{arr}_{idx}_eq_{k}");
        if compound_seeds.iter().any(|(n, _, _)| n == &name) {
            continue;
        }
        let expr_str = format!("{arr}[{idx}] == {k}");
        let Ok(expr) = parse_predicate_expr(&expr_str) else {
            continue;
        };
        compound_seeds.push((name, expr_str, expr));
    }

    // N1 EMERGENT-K — inductive relational-invariant discovery. The frontiers above lift relations from
    // syntactic `eq`/`add` nodes; a design that decides its control return through inequalities or
    // arithmetic carries the deciding relation (`data == target`, `target == data + 1`) only semantically,
    // with NO node to lift — the cube then abstains at ⊥ on the wide datapath (the UF-wrap havoc admits
    // spurious states the missing relation would refute). `discover_inductive_relational_invariants` finds
    // those relations by checking, over the EXACT transition, which register-pair differences are
    // invariant, and seeds them the same compound-predicate way. Deduped against the syntactic seeds by
    // expr string (a relation both a node and the difference-check yield is seeded once).
    let discovery_budget = (MAX_AUTO_SEED + 1).saturating_sub(specs.len() + compound_seeds.len());
    for (name, expr_str, expr) in
        discover_inductive_relational_invariants(&file, &init_values, &good_coi, discovery_budget)
    {
        if compound_seeds.iter().any(|(_, e, _)| e == &expr_str) {
            continue; // a syntactic frontier already seeded this exact relation
        }
        compound_seeds.push((name, expr_str, expr));
    }

    // NOTE (emergent-K, 2026-07-13): the interpolation discovery
    // (`refine::discover_relational_predicates`) is VALIDATED for safety (it uniquely
    // finds constant bounds `reg <= K` the pair-search + Houdini miss), but it
    // interpolates against `¬bad` — recoverability designs carry a `good` TARGET and
    // NO `bad` node, so upfront seeding here is inert. The correct integration is
    // REFINEMENT-based (interpolate the reachable states against the ⊥ failure-subgame
    // in the CEGAR loop), a heavier change deferred until a real branching ⊥ case
    // demonstrates it is needed (the mature difference/ordering/ranking/Houdini
    // machinery decided every case probed so far). See the plan §9 P2.4.

    // IC3ia I1 — HOUDINI relative-inductive conjunction. The difference-discovery above keeps only
    // INDIVIDUALLY 1-inductive relations; some designs need a CONJUNCTIVE invariant `P1 ∧ P2` whose
    // conjuncts are inductive only *relative to each other* (a swap/coupling where `x>=1` holds only
    // because `y>=1`, and vice-versa). Enumerate simple bound candidates (`r >= 1` per good-cone state
    // register) and keep the largest relatively-inductive sub-conjunction; seed each survivor as a
    // compound predicate. SOUND: an inductive conjunction over-approximates the reachable set.
    //
    // GATE (cost): only run this (the O(candidates²)-SMT fallback) when the CHEAP relational discovery
    // found NOTHING — if a difference/addend relation was already lifted, it is almost always what the
    // cube needs, and Houdini's conjunctive search would just add SMT cost. The ranking certificate has
    // already returned for the ranking class, so this fires only on cube-path designs with no cheap
    // relational seed (exactly the swap/coupling fallback the diagnosis identified).
    if compound_seeds.is_empty() && specs.len() < MAX_AUTO_SEED {
        let candidates: Vec<PredicateExpr> = init_values
            .keys()
            .filter(|r| in_coi(r))
            .take(MAX_AUTO_SEED)
            .map(|r| PredicateExpr::Cmp {
                register: r.clone(),
                op: CmpOp::Ge,
                value: 1,
            })
            .collect();
        if candidates.len() >= 2 {
            for expr in houdini_inductive_conjunction(&file, &init_values, candidates) {
                if specs.len() + compound_seeds.len() > MAX_AUTO_SEED {
                    break;
                }
                let PredicateExpr::Cmp {
                    register,
                    op: CmpOp::Ge,
                    value,
                } = &expr
                else {
                    continue;
                };
                let expr_str = format!("{register} >= {value}");
                if compound_seeds.iter().any(|(_, e, _)| e == &expr_str) {
                    continue;
                }
                compound_seeds.push((format!("houdini_{register}_ge_{value}"), expr_str, expr));
            }
        }
    }

    // name → expr, for the reset-cube evaluation + free-input guard of the compound predicates.
    let compound_map: std::collections::HashMap<String, PredicateExpr> = compound_seeds
        .iter()
        .map(|(name, _, expr)| (name.clone(), expr.clone()))
        .collect();

    // AG EF good, over the PREDICATE-NAME atom (the cube path resolves `good` to the
    // `good` predicate's 3-valued label, not a raw register comparison).
    let formula = mu_parser::parse("nu Y. ((mu X. (good || <> X)) && [] Y)")
        .map_err(|e| format!("building the AG EF cube formula: {e:?}"))?;

    // SOUNDNESS: pin each predicate's register to its BTOR2 reset value via
    // `config_values`, so the cube lift's initial cube is the design's reset state.
    // WITHOUT this pin the lift defaults its initial cube to all-false, which is NOT
    // the reset state and can falsely report VIOLATED — the pin is mandatory. A
    // predicate whose register has no `init_values` entry (e.g. a free input) is
    // silently skipped, which is correct (a free input has no reset value to pin).
    let mut config_entries: Vec<String> = specs
        .iter()
        .filter_map(|s| {
            init_values
                .get(&s.register)
                .map(|v| format!("{}={}", s.register, v))
        })
        .collect();
    // Pin the relational predicates' registers to their reset values too (both operands are states),
    // so the lift's initial cube is the design's reset state for the compound bits as well.
    for (_, _, expr) in &compound_seeds {
        for reg in expr.registers() {
            if let Some(v) = init_values.get(&reg) {
                let entry = format!("{reg}={v}");
                if !config_entries.contains(&entry) {
                    config_entries.push(entry);
                }
            }
        }
    }
    let base_sidecar_json = config_values_to_sidecar_json(&config_entries)
        .map_err(|e| format!("recoverability cube path: building the config-values pin: {e}"))?;
    // Inject the relational atoms as sidecar `compound_predicates` (the CEGAR loop reads them via
    // `sidecar_compound_predicates`, adds each as a cube dimension, and forces the compound-aware
    // SmtAllPairs+Eager lift). A `None` base (no config entries) still needs the compound array.
    let sidecar_json = if compound_seeds.is_empty() {
        base_sidecar_json
    } else {
        let mut val: serde_json::Value = match &base_sidecar_json {
            Some(s) => serde_json::from_str(s).map_err(|e| {
                format!("recoverability cube path: re-parsing the config-values sidecar: {e}")
            })?,
            None => serde_json::json!({ "module": "cegar", "source": "cegar.btor2" }),
        };
        let decls: Vec<serde_json::Value> = compound_seeds
            .iter()
            .map(|(name, expr, _)| {
                serde_json::json!({ "name": name, "expr": expr, "derived": false })
            })
            .collect();
        val["compound_predicates"] = serde_json::Value::Array(decls);
        Some(val.to_string())
    };
    let adapter_options = AdapterOptions {
        sidecar_json,
        ..Default::default()
    };

    // PERFORMANCE guard (2026-07-11, no longer a soundness band-aid): abstain on a UF-wrapping design
    // ONLY when the CALLER supplied extra predicates. Rationale:
    //   - Soundness is handled by the universal-hyper-must ◇ (PR #302): an inflated (havoc'd) target
    //     set only pushes ◇ toward `⊥`, never fabricates a `Holds`. So running the wrapped cube is
    //     always SOUND. This guard is purely about cost.
    //   - The AUTO path seeds only the good register's CONTROL states (above). Those predicates are
    //     over the controller, not the wide-op OUTPUT register, so the wide op stays on the may side
    //     (UF-wrapped) and never enters the cube successor / must query — the cube is small and FAST.
    //     Empirically it now DECIDES the wide-datapath control-return class (pos48 `Holds` ~1.4s,
    //     mult48 — a 48-bit MULTIPLIER — `Holds` ~0.12s; trap48 soundly `⊥`), where the exact BDD
    //     walls on the multiplier. This is the N1-first-increment payoff, so we RUN it.
    //   - A caller-supplied extra predicate MIGHT be over the wide-op output register (e.g.
    //     `data == 0`), which forces the wide op into the cube successor computation → the
    //     `O(2^{2|P|})` all-pairs SMT over wide arithmetic can be slow. Rather than risk a hang we
    //     ABSTAIN in that case (conservative; sound). A precise "abstain iff an extra predicate is in
    //     a wrapped-op's cone" is a follow-up; today "caller passed extras on a wrapped design" is the
    //     safe proxy.
    if !extra_predicates.is_empty()
        && !crate::adapter::btor2::bit_blast::collect_uf_wrapped_nids(&file, &adapter_options)
            .is_empty()
    {
        return Ok(PropertyVerdict::Unknown);
    }

    // Cube + smt-hyper-must, matching the verify_auto CegarOptions shape.
    let cegar_opts = CegarOptions {
        max_iterations: RECOVERABILITY_MAX_ITERATIONS,
        // F.1 (2026-07-24) — the transition-aware Craig refinement (discovers the relational/bound
        // invariant a datapath-dependent ⊥ needs). The source is chosen by the CALLER
        // (front-B seam): `default_craig_source()` for CLI/API/tests (env-gated, WP default), or
        // the planner's MathSAT-aware decision. The loop always falls back to WP, so an explicit
        // `Craig` can only DECIDE MORE (sound + monotone under SmtHyperMust).
        predicate_source,
        max_cube_count: 1024,
        capture_approximants: false,
        enable_approximant_reuse: false,
        smart_uf_cap: true,
        lift_strategy: LiftStrategy::Eager,
        // The sound νμ hyper-must (GKMTS ∀∃ over the may-successor set) — definite
        // recoverability verdicts transfer to the concrete design (Bruns–Godefroid /
        // Shoham–Grumberg).
        must_edge_inference: MustEdgeInference::SmtHyperMust,
        // The sound all-pairs SMT may-relation (over-approximation).
        may_edge_inference: MayEdgeInference::SmtAllPairs,
        emit_ctxdsl: false,
    };
    // The cube space includes the compound (relational) predicates the CEGAR loop adds as dimensions.
    let env = Environment::new(1usize << (specs.len() + compound_seeds.len()));

    // DIAG (never merge): breadcrumbs around the ladder's long phase, so the last line before the
    // CI-only abort says which phase it died in.
    eprintln!(
        "[diag overflow] ladder on `AG EF {good}`: {} specs + {} compound seeds; entering cegar_refine_loop",
        specs.len(),
        compound_seeds.len()
    );
    let trace = match cegar_refine_loop(
        &formula,
        btor2_content,
        specs.clone(),
        &env,
        &adapter_options,
        &cegar_opts,
    ) {
        Ok(t) => t,
        // SOUNDNESS: a CEGAR error (SMT failure, cube-cap overflow, …) is an honest
        // abstain — never a fabricated verdict, matching the exact verb's
        // abstain-on-error posture.
        Err(_) => return Ok(PropertyVerdict::Unknown),
    };
    eprintln!("[diag overflow] ladder on `AG EF {good}`: cegar_refine_loop returned");

    // The design's initial cube(s): evaluate every FINAL predicate at the reset valuation, in the
    // lift's cube-bit order (`final_predicates[i]` ↔ bit `i`). Three kinds of bit:
    //   - PINNED: every register the predicate reads has an `init` line → one fixed truth value.
    //   - FREE-INIT: some register it reads has NO `init` line (free at cycle 0 — BTOR2 semantics,
    //     mununu#505), or is an input WeakestPrecondition refinement pulled in (free at cycle 0 too).
    //     EVERY value of such a register is a REAL initial state, so the bit is enumerated over both
    //     truth values and every flavour is trusted in BOTH directions (a `False` flavour is a real
    //     violated initial state; `Holds` needs every flavour `True`). Pinning it to 0 — what the
    //     defaulted valuation did until 2026-10-06 — fabricated a definite HOLDS on a held-register
    //     relational design whose exact verdict is VIOLATED.
    //   - SEL: a Select's reset truth (`mem[key] == K` at cycle 0) depends on the array's reset
    //     CONTENT, which `init_values` (BV-only) does not model. The array is FREE at reset but that
    //     over-APPROXIMATES the initial set, so only a UNANIMOUS `True` is trusted (never `Violated`).
    let init_map: std::collections::HashMap<String, u128> =
        init_values.iter().map(|(k, v)| (k.clone(), *v)).collect();
    let mut init_cube = 0usize;
    let mut free_init_bits: Vec<usize> = Vec::new();
    let mut free_select_bits: Vec<usize> = Vec::new();
    for (i, spec) in trace.final_predicates.iter().enumerate() {
        match compound_map.get(&spec.name) {
            Some(expr) if expr.has_select() => free_select_bits.push(i),
            Some(expr) => {
                if !expr.registers().iter().all(|r| init_values.contains_key(r)) {
                    free_init_bits.push(i);
                } else if expr.eval(&init_map) {
                    init_cube |= 1 << i;
                }
            }
            None => match init_values.get(&spec.register) {
                None => free_init_bits.push(i),
                Some(v) if *v == spec.value as u128 => init_cube |= 1 << i,
                Some(_) => {}
            },
        }
    }
    Ok(reset_cube_verdict(
        |cube| trace.final_verdict.verdict_at(cube),
        init_cube,
        &free_init_bits,
        &free_select_bits,
    ))
}

/// The verdict at the design's initial cube(s), enumerating the bits whose reset truth is not pinned.
///
/// `free_init_bits` are predicate bits over a register with no `init` (or an input): every flavour
/// is a REAL initial state, so a `False` flavour is a sound `Violated` and `Holds` needs all `True`.
/// `free_select_bits` are array-content bits: the free content OVER-approximates the initial set, so
/// across those flavours only a unanimous `True` is trusted — a `False` there may be an initial
/// array content the design never has, and is read as `Unknown`. Bounded: more than
/// `MAX_FREE_RESET_BITS` free bits abstain (the 2^k enumeration stays small).
fn reset_cube_verdict(
    verdict_at: impl Fn(usize) -> Trit,
    init_cube: usize,
    free_init_bits: &[usize],
    free_select_bits: &[usize],
) -> PropertyVerdict {
    const MAX_FREE_RESET_BITS: usize = 4;
    if free_init_bits.len() + free_select_bits.len() > MAX_FREE_RESET_BITS {
        return PropertyVerdict::Unknown;
    }
    let with_bits = |base: usize, bits: &[usize], mask: usize| -> usize {
        let mut cube = base;
        for (b, &bit) in bits.iter().enumerate() {
            if (mask >> b) & 1 == 1 {
                cube |= 1 << bit;
            }
        }
        cube
    };
    let mut all_true = true;
    let mut any_false = false;
    for imask in 0..(1usize << free_init_bits.len()) {
        let base = with_bits(init_cube, free_init_bits, imask);
        // Over the select flavours this init flavour is: True only if unanimously True; False only
        // if there are NO select bits (no over-approximation in play); otherwise Unknown.
        let mut sel_all_true = true;
        let mut sel_any_false = false;
        for smask in 0..(1usize << free_select_bits.len()) {
            match verdict_at(with_bits(base, free_select_bits, smask)) {
                Trit::True => {}
                Trit::False => {
                    sel_all_true = false;
                    sel_any_false = true;
                }
                Trit::Unknown => sel_all_true = false,
            }
        }
        if !sel_all_true {
            all_true = false;
        }
        if sel_any_false && free_select_bits.is_empty() {
            any_false = true;
        }
    }
    if all_true {
        PropertyVerdict::Holds
    } else if any_false {
        PropertyVerdict::Violated
    } else {
        PropertyVerdict::Unknown
    }
}

/// Parse an extra-abstraction-predicate triple `NAME:REGISTER=VALUE` (the surface
/// syntax shared by `btor2 verify-recoverability --predicate` and the API
/// `predicates` field) into a [`PredicateSpec`]. Same shape as `btor2 cegar`.
pub fn parse_extra_predicate(raw: &str) -> Result<PredicateSpec, String> {
    let (name, rest) = raw.split_once(':').ok_or_else(|| {
        format!("predicate spec '{raw}' missing ':' separator (expected NAME:REGISTER=VALUE)")
    })?;
    let (register, value_str) = rest.split_once('=').ok_or_else(|| {
        format!("predicate spec '{raw}' missing '=' separator (expected NAME:REGISTER=VALUE)")
    })?;
    let value: u64 = value_str
        .parse()
        .map_err(|e| format!("predicate spec '{raw}' has non-numeric value: {e}"))?;
    Ok(PredicateSpec {
        name: name.to_string(),
        register: register.to_string(),
        value,
    })
}

/// The `AG EF good` formula string this command decides, for provenance / echoing
/// on a surface (`AG EF (<good>)`).
pub fn recoverability_property_str(good: &str) -> String {
    format!("AG EF ({good})")
}

/// Append a fresh 1-bit combinational node that is the disjunction of every `bad` condition, carrying a
/// stable symbol so the predicate-cube can reference `bad` as a DERIVED (combinational) predicate.
/// Returns `(rewritten_btor2, bad_symbol, has_constraints)`. `None` when the design has no `bad` line.
///
/// The disjunction is the safety obligation (ANY `bad` reachable ⇒ unsafe). `has_constraints` flags
/// whether the design carries `constraint` (environment-assumption) lines: the cube encoding ignores
/// them (it over-approximates by dropping the assumption), which keeps a `Holds` verdict SOUND (safe
/// under more behaviours ⇒ safe under the assumed subset) but makes a `Violated` verdict suspect (the
/// counterexample may violate an assumption) — the caller downgrades `Violated` to `Unknown` then.
fn inject_bad_symbol(btor2: &str, file: &crate::adapter::btor2::ast::Btor2File) -> Option<String> {
    use crate::adapter::btor2::ast::{Node, Sort};
    let bad_conds: Vec<i64> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Bad { signal } => Some(signal.0.abs()),
            _ => None,
        })
        .collect();
    if bad_conds.is_empty() {
        return None;
    }
    // A 1-bit sort node (every `bad` is 1-bit, so one exists).
    let sort1 = file.lines.iter().find_map(|l| match &l.node {
        Node::Sort {
            sort: Sort::BitVec { width: 1 },
        } => Some(l.nid),
        _ => None,
    })?;
    let mut nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut out = String::from(btor2);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    // Fold the bad conditions into a single OR chain; the final node gets the `__mununu_bad` symbol.
    let mut acc = bad_conds[0];
    if bad_conds.len() == 1 {
        // `or C C` == C — a uniform way to give the single condition a named node.
        out.push_str(&format!("{nid} or {sort1} {acc} {acc} __mununu_bad\n"));
    } else {
        for (i, c) in bad_conds[1..].iter().enumerate() {
            let last = i == bad_conds.len() - 2;
            if last {
                out.push_str(&format!("{nid} or {sort1} {acc} {c} __mununu_bad\n"));
            } else {
                out.push_str(&format!("{nid} or {sort1} {acc} {c}\n"));
                acc = nid;
                nid += 1;
            }
        }
    }
    Some(out)
}

/// Decide a BTOR2 **safety** obligation (`bad` unreachable) with the KMTS 3-valued predicate cube —
/// the translation that lets the branching-cube engine (and its inductive relational-invariant
/// discovery) attack a `bad`-state property. `bad` is translated to the modal-μ safety formula
/// `AG ¬bad = nu X. ((not bad) and [] X)`, with `bad` carried as a DERIVED combinational predicate and
/// the abstraction seeded from the design's guard atoms PLUS
/// [`discover_inductive_relational_invariants`] (the emergent-K invariant discovery).
///
/// Verdict mapping (over the reset cube, Bruns–Godefroid definite-verdict transfer):
/// - `Holds` ⇒ **safe** (`bad` unreachable) — sound even ignoring `constraint` lines (over-approx);
/// - `Violated` ⇒ **`bad` reachable** via a must-path — downgraded to `Unknown` when the design has
///   `constraint` lines (the must-path may violate an assumption; conservative, sound);
/// - `Unknown` ⇒ the bounded predicate cube abstains (the honest boundary).
///
/// This is the evaluation surface for "how far does the 3-valued cube + invariant discovery reach on
/// real safety benchmarks (HWMCC)". It is deliberately a thin translation over the audited cube path.
/// Format an **atomic** [`PredicateExpr`] (a single `Cmp` / `CmpReg` / `CmpRegAddend`
/// leaf) back into a `parse_predicate_expr`-round-trippable string — the form the
/// sidecar `compound_predicates` entry carries. Returns `None` for boolean compounds
/// (`And` / `Or` / `Not`), which the safety cube does not seed as single dimensions.
fn atomic_expr_string(expr: &PredicateExpr) -> Option<String> {
    fn op(op: CmpOp) -> &'static str {
        match op {
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        }
    }
    match expr {
        PredicateExpr::Cmp {
            register,
            op: o,
            value,
        } => Some(format!("{register} {} {value}", op(*o))),
        PredicateExpr::CmpReg { lhs, op: o, rhs } => Some(format!("{lhs} {} {rhs}", op(*o))),
        PredicateExpr::CmpRegAddend {
            lhs,
            op: o,
            rhs,
            addend,
            ..
        } => Some(format!("{lhs} {} {rhs} + {addend}", op(*o))),
        PredicateExpr::Select {
            array,
            index,
            op: o,
            value,
        } => Some(format!("{array}[{index}] {} {value}", op(*o))),
        PredicateExpr::And(..) | PredicateExpr::Or(..) | PredicateExpr::Not(..) => None,
    }
}

pub fn verify_safety_scalable(btor2_content: &str) -> Result<PropertyVerdict, String> {
    let file = crate::adapter::btor2::parser::parse(btor2_content)
        .map_err(|e| format!("safety cube path: parsing BTOR2: {}", e.message))?;
    let has_constraints = file
        .lines
        .iter()
        .any(|l| matches!(l.node, crate::adapter::btor2::ast::Node::Constraint { .. }));
    let Some(rewritten) = inject_bad_symbol(btor2_content, &file) else {
        return Err("safety cube path: BTOR2 has no `bad` property".to_string());
    };
    let file = crate::adapter::btor2::parser::parse(&rewritten).map_err(|e| {
        format!(
            "safety cube path: re-parsing translated BTOR2: {}",
            e.message
        )
    })?;
    let init_values: BTreeMap<String, u128> =
        crate::adapter::btor2::concrete_oracle::init_valuation(&file);

    const MAX_AUTO_SEED: usize = 8;
    // Cube dimensions: the design's `register == constant` guard atoms (the values the control logic
    // compares against), capped. If none, bootstrap with a few state registers pinned at their reset
    // value so the cube has ≥1 dimension for the CEGAR loop to refine from.
    let mut specs: Vec<PredicateSpec> = Vec::new();
    for (reg, k) in eq_guard_atoms(&file) {
        if specs.len() >= MAX_AUTO_SEED {
            break;
        }
        if !specs.iter().any(|s| s.register == reg && s.value == k) {
            specs.push(PredicateSpec {
                name: format!("guard_{reg}_eq_{k}"),
                register: reg,
                value: k,
            });
        }
    }
    if specs.is_empty() {
        for (reg, v) in init_values.iter().take(MAX_AUTO_SEED) {
            if let Ok(value) = u64::try_from(*v) {
                specs.push(PredicateSpec {
                    name: format!("reset_{reg}"),
                    register: reg.clone(),
                    value,
                });
            }
        }
    }

    // Relational / inductive compound predicates: the syntactic relational guard atoms PLUS the
    // emergent-K inductive difference-invariant discovery (unrestricted cone for safety).
    let empty_cone: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut compound_seeds: Vec<(String, String, PredicateExpr)> = Vec::new();
    for (lhs, rhs) in eq_reg_guard_atoms(&file) {
        if specs.len() + compound_seeds.len() >= MAX_AUTO_SEED {
            break;
        }
        let expr_str = format!("{lhs} == {rhs}");
        if let Ok(expr) = parse_predicate_expr(&expr_str)
            && !compound_seeds.iter().any(|(_, e, _)| e == &expr_str)
        {
            compound_seeds.push((format!("rel_{lhs}_eq_{rhs}"), expr_str, expr));
        }
    }
    let discovery_budget = (MAX_AUTO_SEED + 1).saturating_sub(specs.len() + compound_seeds.len());
    for (name, expr_str, expr) in
        discover_inductive_relational_invariants(&file, &init_values, &empty_cone, discovery_budget)
    {
        if !compound_seeds.iter().any(|(_, e, _)| e == &expr_str) {
            compound_seeds.push((name, expr_str, expr));
        }
    }
    // Emergent-K interpolation discovery — iterative forward Craig interpolation over the exact
    // transition finds CONSTANT BOUNDS (`reg < K`) and register ORDERINGS the difference/eq search
    // structurally misses (it never compares a register to a literal). The safety path has a `bad`
    // node (via `inject_bad_symbol`), so the discovery is live here (unlike the recoverability path,
    // which has no `bad`). Seeds are HINTS: the CEGAR loop independently re-verifies each verdict, so
    // a spurious over-approximation on an unsafe design is rejected (sound abstain), never a false
    // Holds.
    //
    // LAST-RESORT gating (mirrors `native_interp`'s role in `reach_portfolio`). The discovery spawns
    // cvc5 and costs ~7.5s; a broad HWMCC/OpenTitan sweep (2026-07-13) measured ZERO decide-lift when
    // it ran on well-seeded designs (the residual abstentions there are deep-CEX-unsafe / BMC-depth
    // gaps, not missing-predicate gaps). So only pay it when the cheap syntactic + difference paths
    // left the cube RELATIONALLY under-constrained — `compound_seeds` below the watermark — which is
    // the only regime where a discovered bound/ordering can add a genuinely new dimension. A cube
    // already carrying relational structure skips the expensive step (sub-second, unchanged).
    const DISCOVERY_LAST_RESORT_WATERMARK: usize = 2;
    if compound_seeds.len() < DISCOVERY_LAST_RESORT_WATERMARK
        && specs.len() + compound_seeds.len() < MAX_AUTO_SEED
        && std::env::var_os("MUNUNU_NO_INTERP_DISCOVERY").is_none()
    {
        for expr in crate::adapter::btor2::refine::discover_relational_predicates(&file, 8, 4_000) {
            if specs.len() + compound_seeds.len() >= MAX_AUTO_SEED {
                break;
            }
            let Some(expr_str) = atomic_expr_string(&expr) else {
                continue;
            };
            if compound_seeds.iter().any(|(_, e, _)| e == &expr_str) {
                continue;
            }
            let name = format!("interp_{}", compound_seeds.len());
            compound_seeds.push((name, expr_str, expr));
        }
    }
    let compound_map: std::collections::HashMap<String, PredicateExpr> = compound_seeds
        .iter()
        .map(|(name, _, expr)| (name.clone(), expr.clone()))
        .collect();

    // AG ¬bad, over the `bad` derived-predicate atom.
    let formula = mu_parser::parse("nu X. ((not bad) and [] X)")
        .map_err(|e| format!("safety cube path: building the AG ¬bad formula: {e:?}"))?;

    // Pin each seed predicate's register to its reset value (mandatory — an unpinned initial cube
    // defaults to all-false, not the reset state, and can falsely report Violated).
    let mut config_entries: Vec<String> = specs
        .iter()
        .filter_map(|s| {
            init_values
                .get(&s.register)
                .map(|v| format!("{}={}", s.register, v))
        })
        .collect();
    for (_, _, expr) in &compound_seeds {
        for reg in expr.registers() {
            if let Some(v) = init_values.get(&reg) {
                let entry = format!("{reg}={v}");
                if !config_entries.contains(&entry) {
                    config_entries.push(entry);
                }
            }
        }
    }
    let base_sidecar_json = config_values_to_sidecar_json(&config_entries)
        .map_err(|e| format!("safety cube path: building the config-values pin: {e}"))?;
    // Sidecar: the `bad` derived predicate + the relational compound predicates.
    let mut val: serde_json::Value = match &base_sidecar_json {
        Some(s) => serde_json::from_str(s)
            .map_err(|e| format!("safety cube path: re-parsing the config-values sidecar: {e}"))?,
        None => serde_json::json!({ "module": "safety", "source": "safety.btor2" }),
    };
    val["combinational_predicates"] = serde_json::json!([
        { "name": "bad", "signal": "__mununu_bad", "value": 1 }
    ]);
    if !compound_seeds.is_empty() {
        let decls: Vec<serde_json::Value> = compound_seeds
            .iter()
            .map(|(name, expr, _)| serde_json::json!({ "name": name, "expr": expr, "derived": false }))
            .collect();
        val["compound_predicates"] = serde_json::Value::Array(decls);
    }
    let adapter_options = AdapterOptions {
        sidecar_json: Some(val.to_string()),
        ..Default::default()
    };

    let cegar_opts = CegarOptions {
        max_iterations: RECOVERABILITY_MAX_ITERATIONS,
        predicate_source: PredicateSource::WeakestPrecondition,
        max_cube_count: 1024,
        capture_approximants: false,
        enable_approximant_reuse: false,
        smart_uf_cap: true,
        lift_strategy: LiftStrategy::Eager,
        must_edge_inference: MustEdgeInference::SmtHyperMust,
        may_edge_inference: MayEdgeInference::SmtAllPairs,
        emit_ctxdsl: false,
    };
    let env = Environment::new(1usize << (specs.len() + compound_seeds.len()));
    let trace = match cegar_refine_loop(
        &formula,
        &rewritten,
        specs.clone(),
        &env,
        &adapter_options,
        &cegar_opts,
    ) {
        Ok(t) => t,
        Err(_) => return Ok(PropertyVerdict::Unknown),
    };

    // Reset-cube verdict. A predicate over a register with no `init` line (free at cycle 0) is
    // enumerated over both truth values — every flavour is a real initial state — rather than
    // pinned to 0, which would claim `AG ¬bad` from ONE initial state (unsound `Holds`). See
    // `reset_cube_verdict`.
    let init_map: std::collections::HashMap<String, u128> =
        init_values.iter().map(|(k, v)| (k.clone(), *v)).collect();
    let mut init_cube = 0usize;
    let mut free_init_bits: Vec<usize> = Vec::new();
    for (i, spec) in trace.final_predicates.iter().enumerate() {
        match compound_map.get(&spec.name) {
            Some(expr) => {
                if !expr.registers().iter().all(|r| init_values.contains_key(r)) {
                    free_init_bits.push(i);
                } else if expr.eval(&init_map) {
                    init_cube |= 1 << i;
                }
            }
            None => match init_values.get(&spec.register) {
                None => free_init_bits.push(i),
                Some(v) if *v == spec.value as u128 => init_cube |= 1 << i,
                Some(_) => {}
            },
        }
    }
    let verdict = reset_cube_verdict(
        |cube| trace.final_verdict.verdict_at(cube),
        init_cube,
        &free_init_bits,
        &[],
    );
    // SOUNDNESS: with `constraint` lines the cube's must-path may violate an assumption, so a
    // `Violated` is not trustworthy — downgrade to `Unknown`. `Holds` stays (over-approx is sound).
    if has_constraints && verdict == PropertyVerdict::Violated {
        return Ok(PropertyVerdict::Unknown);
    }
    Ok(verdict)
}

#[cfg(test)]
mod tests {
    use super::*;

    // IC3ia I1 DE-RISK — relative-inductive (Houdini) conjunction discovery. The load-bearing question:
    // can we discover a CONJUNCTIVE invariant `P1 ∧ P2` where NEITHER `Pi` is inductive alone (so the
    // one-shot per-predicate discovery cannot), over mununu's exact transition? A 48-bit SWAP design
    // (`x' = y`, `y' = x`, both init 1) has the invariant `x>=1 ∧ y>=1`: neither conjunct is 1-inductive
    // (proving `x'=y >= 1` needs `y>=1`, and vice-versa), but the pair is. No arithmetic ⇒ no BV-wrap
    // confound; the coupling is purely relational.
    #[test]
    fn ic3ia_i1_houdini_finds_conjunction_neither_conjunct_alone() {
        let swap = "\
1 sort bitvec 48
2 state 1 x
3 state 1 y
4 constd 1 1
5 init 1 2 4
6 init 1 3 4
7 next 1 2 3
8 next 1 3 2
";
        let file = crate::adapter::btor2::parser::parse(swap).expect("parse");
        let init = crate::adapter::btor2::concrete_oracle::init_valuation(&file);
        let x_ge1 = PredicateExpr::Cmp {
            register: "x".into(),
            op: CmpOp::Ge,
            value: 1,
        };
        let y_ge1 = PredicateExpr::Cmp {
            register: "y".into(),
            op: CmpOp::Ge,
            value: 1,
        };
        let x_le1 = PredicateExpr::Cmp {
            register: "x".into(),
            op: CmpOp::Le,
            value: 1,
        };

        // NEITHER conjunct is individually 1-inductive: Houdini over a singleton drops it.
        assert!(
            houdini_inductive_conjunction(&file, &init, vec![x_ge1.clone()]).is_empty(),
            "x>=1 is NOT inductive alone (x'=y with y unconstrained can be 0)"
        );
        assert!(
            houdini_inductive_conjunction(&file, &init, vec![y_ge1.clone()]).is_empty(),
            "y>=1 is NOT inductive alone"
        );

        // The CONJUNCTION is inductive: Houdini keeps BOTH — the relative-induction the one-shot can't do.
        let both = houdini_inductive_conjunction(&file, &init, vec![x_ge1.clone(), y_ge1.clone()]);
        assert_eq!(both.len(), 2, "the pair x>=1 ∧ y>=1 IS inductive together");
        assert!(both.contains(&x_ge1) && both.contains(&y_ge1));

        // A relatively-NON-inductive decoy that HOLDS at init (`x<=1`) is dropped by the fixpoint (not the
        // init filter) — exercising the Houdini removal loop, not just the init prefilter.
        let filtered =
            houdini_inductive_conjunction(&file, &init, vec![x_ge1.clone(), y_ge1.clone(), x_le1]);
        assert_eq!(filtered.len(), 2, "x<=1 is dropped by relative-induction");
        assert!(filtered.contains(&x_ge1) && filtered.contains(&y_ge1));
    }

    #[test]
    fn ic3ia_i1_houdini_seeding_decides_where_one_shot_abstains() {
        // END-TO-END de-risk: an OSCILLATING swap (`x'=y`, `y'=x`, init x=1,y=2) — `x-y` alternates
        // (−1,+1) so the difference-invariant discovery finds nothing, and `x` oscillates in {1,2}, never
        // 0, so `AG EF (x==0)` is VIOLATED. Proving the trap needs `x>=1 ∧ y>=1` — a conjunction NEITHER
        // conjunct of which is 1-inductive. The baseline cube ABSTAINS (Unknown) at 48-bit; the Houdini
        // relative-inductive seeding recovers the conjunction and DECIDES Violated. 8-bit exact oracle agrees.
        let osc = |w: u32| -> String {
            format!(
                "1 sort bitvec {w}\n2 state 1 x\n3 state 1 y\n4 constd 1 1\n5 constd 1 2\n\
                 6 init 1 2 4\n7 init 1 3 5\n8 next 1 2 3\n9 next 1 3 2\n"
            )
        };
        assert_eq!(
            verify_recoverability_scalable(&osc(48), "x == 0", &[]).expect("decides"),
            PropertyVerdict::Violated,
            "48-bit conjunctive-invariant trap decides Violated via Houdini relative-inductive seeding"
        );
        assert_eq!(
            verify_recoverability(&osc(8), "x == 0").expect("exact decides"),
            PropertyVerdict::Violated,
            "8-bit exact oracle agrees with the Houdini-seeded 48-bit Violated"
        );
    }

    // === verify_safety_scalable — the bad → AG ¬bad cube translation ===

    #[test]
    fn safety_cube_decides_tiny_safe_and_unsafe() {
        // SAFE: `a` counts up but CAPS at 4 (a==4 ⇒ a'=a), so `bad = (a==5)` is unreachable. The cube
        // seeds the guard atoms {a==4, a==5} and proves AG ¬(a==5) ⇒ Holds (= safe).
        let safe = "\
1 sort bitvec 1
2 sort bitvec 8
3 state 2 a
4 zero 2
5 init 2 3 4
6 one 2
7 constd 2 4
8 eq 1 3 7
9 add 2 3 6
10 ite 2 8 3 9
11 next 2 3 10
12 constd 2 5
13 eq 1 3 12
14 bad 13
";
        assert_eq!(
            verify_safety_scalable(safe).expect("decides"),
            PropertyVerdict::Holds,
            "capped counter: bad (a==5) unreachable ⇒ safe (Holds)"
        );
        // UNSAFE: `a` counts up freely (0,1,2,3,4,5,…) so `bad = (a==5)` IS reached ⇒ Violated.
        let unsafe_design = "\
1 sort bitvec 1
2 sort bitvec 8
3 state 2 a
4 zero 2
5 init 2 3 4
6 one 2
9 add 2 3 6
11 next 2 3 9
12 constd 2 5
13 eq 1 3 12
14 bad 13
";
        assert_eq!(
            verify_safety_scalable(unsafe_design).expect("decides"),
            PropertyVerdict::Violated,
            "free counter: bad (a==5) reachable ⇒ unsafe (Violated)"
        );
    }

    #[test]
    fn safety_cube_decides_ordering_invariant_via_inequality() {
        // Catch-up counter: `b` SATURATES at MAX (no wrap); `a = ite(a<b, a+1, a)` catches up but never
        // passes `b`. So `a <= b` is a genuine 1-INDUCTIVE invariant, but the difference `a - b` VARIES
        // (0 then −1…) — the difference-invariant discovery misses it. Only the ORDERING inequality
        // `a <= b` proves `bad = (a > b)` unreachable. Decides Holds (= safe) at 48-bit where exact walls.
        let sat = |w: u32| -> String {
            format!(
                "1 sort bitvec 1\n2 sort bitvec {w}\n3 state 2 a\n4 state 2 b\n5 zero 2\n6 one 2\n\
                 7 ones 2\n8 init 2 3 5\n9 init 2 4 5\n10 ult 1 3 4\n11 add 2 3 6\n12 ite 2 10 11 3\n\
                 13 next 2 3 12\n14 eq 1 4 7\n15 add 2 4 6\n16 ite 2 14 4 15\n17 next 2 4 16\n\
                 18 ugt 1 3 4\n19 bad 18\n"
            )
        };
        assert_eq!(
            verify_safety_scalable(&sat(48)).expect("decides"),
            PropertyVerdict::Holds,
            "48-bit ordering-invariant safety decides Holds via the inductive inequality a<=b"
        );
        assert_eq!(
            verify_safety_scalable(&sat(8)).expect("decides"),
            PropertyVerdict::Holds,
            "8-bit ordering-invariant safety also Holds"
        );
        // WRAP variant (soundness guard): `b` WRAPS instead of saturating, so at b==MAX→0 with a high,
        // `a > b` IS reached ⇒ `a <= b` is NOT invariant (the inequality check correctly declines to seed
        // it) and the design is genuinely unsafe ⇒ Violated. (Portfolio ground truth: reachable.)
        let wrap = |w: u32| -> String {
            format!(
                "1 sort bitvec 1\n2 sort bitvec {w}\n3 state 2 a\n4 state 2 b\n5 zero 2\n6 one 2\n\
                 7 init 2 3 5\n8 init 2 4 5\n9 ult 1 3 4\n10 add 2 3 6\n11 ite 2 9 10 3\n12 next 2 3 11\n\
                 13 add 2 4 6\n14 next 2 4 13\n15 ugt 1 3 4\n16 bad 15\n"
            )
        };
        assert_eq!(
            verify_safety_scalable(&wrap(8)).expect("decides"),
            PropertyVerdict::Violated,
            "the wrapping variant is genuinely unsafe (b wraps below a) ⇒ Violated"
        );
    }

    #[test]
    fn safety_cube_decides_constant_bound_via_interpolation_discovery() {
        // The interpolation last-resort discovery (`discover_relational_predicates`, gated behind the
        // relationally-under-constrained watermark in `verify_safety_scalable`) seeds a register-TO-
        // CONSTANT bound (`a <= 4`) that no cheaper path can produce: `eq_guard_atoms` yields the
        // equality `a == 4` (not a bound); the difference/ordering discovery compares register PAIRS
        // (a single-register design has none); and the CEGAR loop's WeakestPrecondition refinement
        // ALSO cannot get there (measured: with the discovery disabled via MUNUNU_NO_INTERP_DISCOVERY
        // the same design is Unknown, not Holds). `bad = (a > 4)` on a counter that CAPS at 4 is
        // therefore decided Holds ONLY once the discovered bound seeds the cube — this test fails
        // (→ Unknown) if the last-resort wiring is removed.
        let cap_gt = "\
1 sort bitvec 1
2 sort bitvec 8
3 state 2 a
4 zero 2
5 init 2 3 4
6 one 2
7 constd 2 4
8 eq 1 3 7
9 add 2 3 6
10 ite 2 8 3 9
11 next 2 3 10
12 ugt 1 3 7
13 bad 12
";
        // SOUNDNESS (always valid, cvc5 present or not): the FREE counter (no cap) genuinely reaches
        // a>4. The discovery must NEVER yield a false Holds — with cvc5 the verdict-verified CEGAR
        // rejects the spurious over-approximation (measured: Unknown); without cvc5 no bound is seeded
        // (also Unknown). Either way `!= Holds`. Guards the exact failure mode a HINT-generator risks:
        // a plausible-but-false invariant becoming a wrong `safe`.
        let free_gt = "\
1 sort bitvec 1
2 sort bitvec 8
3 state 2 a
4 zero 2
5 init 2 3 4
6 one 2
9 add 2 3 6
11 next 2 3 9
7 constd 2 4
12 ugt 1 3 7
13 bad 12
";
        assert_ne!(
            verify_safety_scalable(free_gt).expect("decides"),
            PropertyVerdict::Holds,
            "free counter reaches a>4 — the discovered bound must never yield a false Holds"
        );
        // DECIDE-LIFT (cvc5-gated — cvc5 is a subprocess tool, NOT bundled in the mununu-dev CI image,
        // so `discover_relational_predicates` returns no predicates there and the cube abstains). When
        // cvc5 is present, `a <= 4` is discovered and decides Holds; when absent, the verdict is
        // Unknown and we skip (matching the `#[ignore]`-free cvc5-absence guard in refine.rs's tests).
        let cap_verdict = verify_safety_scalable(cap_gt).expect("decides");
        if cap_verdict == PropertyVerdict::Unknown {
            eprintln!("SKIP (cvc5 absent): no discovered bound a<=4 ⇒ the safety cube abstains");
            return;
        }
        assert_eq!(
            cap_verdict,
            PropertyVerdict::Holds,
            "capped counter bad=(a>4) decides Holds via the interpolation-discovered bound a<=4"
        );
    }

    // 3-state responder: st 0=idle, 1=req, 2=grant; idle -go-> req; req -> grant;
    // grant -> idle. Every reachable state can reach idle ⇒ AG EF (st==0) HOLDS.
    const RESPONDER: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 state 1 st
4 zero 1
5 init 1 3 4
6 input 2 go
7 one 1
8 constd 1 2
9 eq 2 3 4
10 eq 2 3 7
11 ite 1 6 7 4
12 ite 1 10 8 4
13 ite 1 9 11 12
14 next 1 3 13
";

    // 4-state staller: st 0=idle, 1=req, 3=stuck (absorbing); 2=grant unreachable.
    // idle -go-> req; req -> stuck; stuck -> stuck. The reachable `stuck` cannot get
    // back to idle ⇒ AG EF (st==0) VIOLATED.
    const STALLER: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 state 1 st
4 zero 1
5 init 1 3 4
6 input 2 go
7 one 1
8 constd 1 2
9 constd 1 3
10 eq 2 3 4
11 eq 2 3 7
12 ite 1 6 7 4
13 ite 1 11 9 3
14 ite 1 10 12 13
15 next 1 3 14
";

    #[test]
    fn recoverable_design_holds() {
        assert_eq!(
            verify_recoverability(RESPONDER, "st == 0").expect("decides"),
            PropertyVerdict::Holds
        );
    }

    #[test]
    fn design_with_absorbing_trap_is_violated() {
        assert_eq!(
            verify_recoverability(STALLER, "st == 0").expect("decides"),
            PropertyVerdict::Violated
        );
    }

    #[test]
    fn malformed_target_errors() {
        assert!(verify_recoverability(RESPONDER, "not an atom !!").is_err());
    }

    #[test]
    fn property_string_echoes_the_target() {
        assert_eq!(recoverability_property_str("st == 0"), "AG EF (st == 0)");
    }

    // An 80-bit free-running counter (over the 64-bit auto-cap ceiling) whose value
    // gates a small 2-state FSM (`st`: 0=idle, 1=busy). The counter feeds st's
    // next-state (idle advances to busy only when `cnt == 0`), so the counter is IN
    // st's cone-of-influence → the exact engine over-caps. busy ALWAYS returns to idle,
    // so every reachable state can reach idle ⇒ AG EF (st==0) HOLDS.
    //
    // Width note: this MUST exceed AUTO_CAP_CEILING (192, raised from 64 in P2.5-A), not just the
    // 40-bit floor. For an `AG EF (control-target)` property the EF-reachability fixpoint converges
    // in ~2 iterations regardless of the counter's width (a free-running counter never *blocks*
    // reaching idle), so the fixpoint iteration budget never fires — only the bit cap does. At
    // ≤ 192 bits the auto-cap would admit the cone and exact would DECIDE (a win), defeating the
    // "exact abstains" premise this test needs.
    const WIDE_RECOVERABLE: &str = "\
1 sort bitvec 300
2 sort bitvec 2
3 sort bitvec 1
4 state 1 cnt
5 zero 1
6 init 1 4 5
7 inc 1 4
8 next 1 4 7
9 state 2 st
10 zero 2
11 init 2 9 10
12 one 2
13 eq 3 9 10
14 eq 3 4 5
15 ite 2 14 12 10
16 ite 2 13 15 10
17 next 2 9 16
";

    // Same 300-bit counter (over the 192-bit auto-cap ceiling, per WIDE_RECOVERABLE's width note)
    // but st's next-state is `stuck (2)` UNCONDITIONALLY — the `ite(cnt==0, 2, 2)` keeps the
    // counter syntactically in st's cone (so the exact engine over-caps) while every state moves
    // to the absorbing trap. From `stuck`, idle is unreachable ⇒ AG EF (st==0) VIOLATED.
    const WIDE_TRAP: &str = "\
1 sort bitvec 300
2 sort bitvec 2
3 sort bitvec 1
4 state 1 cnt
5 zero 1
6 init 1 4 5
7 inc 1 4
8 next 1 4 7
9 state 2 st
10 zero 2
11 init 2 9 10
12 constd 2 2
13 eq 3 4 5
14 ite 2 13 12 12
15 next 2 9 14
";

    /// The **exact-engine-only** verdict (no cube escalation) — the differential
    /// oracle. `Unknown` means the exact engine abstained (over-cap / unsupported).
    fn exact_verdict(btor2: &str, good: &str) -> PropertyVerdict {
        let formula_str = format!("nu Y. ((mu X. (({good}) || <> X)) && [] Y)");
        let formula = mu_parser::parse(&formula_str).expect("AG EF formula parses");
        match exact_symbolic_verdict(btor2, &formula) {
            Ok(v) => PropertyVerdict::from(v),
            Err(_) => PropertyVerdict::Unknown,
        }
    }

    /// Two held `n`-bit registers `a`, `b` and a 1-bit `done` latched on `a == b`. With `init`
    /// = `None` the registers have NO `init` line — free at cycle 0 (BTOR2; mununu#505) — so
    /// `AG EF (done == 1)` is VIOLATED (an initial `a ≠ b` never sets `done`). With explicit
    /// inits the verdict is pinned: `a ≠ b` → VIOLATED, `a == b` → HOLDS.
    fn held_pair_done_latch(n: u32, init: Option<(u64, u64)>) -> String {
        let mut src = format!(
            "1 sort bitvec 1\n2 sort bitvec {n}\n3 state 2 a\n4 state 2 b\n5 next 2 3 3\n6 next 2 4 4\n\
             7 eq 1 3 4\n8 state 1 done\n9 zero 1\n10 init 1 8 9\n11 or 1 8 7\n12 next 1 8 11\n"
        );
        if let Some((a0, b0)) = init {
            src.push_str(&format!(
                "13 constd 2 {a0}\n14 constd 2 {b0}\n15 init 2 3 13\n16 init 2 4 14\n"
            ));
        }
        src
    }

    /// 2026-10-06 — a register with no `init` line is FREE at cycle 0, and the cube path's reset
    /// cube used to pin it to 0 (`init_valuation`'s `unwrap_or(0)`), fabricating a definite HOLDS
    /// that the exact engine refutes. Found while calibrating profiling cases: the exact engine
    /// abstained on an OxiDD arena exhaustion, the escalation took over, and the run printed
    /// `holds`. The ladder must agree with the exact oracle on its own.
    #[test]
    fn free_init_registers_are_enumerated_not_pinned_in_the_recoverability_reset_cube() {
        let free = held_pair_done_latch(6, None);
        assert_eq!(
            exact_verdict(&free, "done == 1"),
            PropertyVerdict::Violated,
            "exact oracle"
        );
        let ladder = verify_recoverability_scalable(&free, "done == 1", &[]).expect("ladder runs");
        assert_ne!(
            ladder,
            PropertyVerdict::Holds,
            "the scalable ladder claimed HOLDS from a reset cube that pinned free registers to 0"
        );
        assert_eq!(
            ladder,
            PropertyVerdict::Violated,
            "every value of a free-init register is a real initial state, so the `a ≠ b` flavour is a sound VIOLATED"
        );
        // Explicit inits: the pinned reset cube decides both ways, unchanged.
        assert_eq!(
            verify_recoverability_scalable(
                &held_pair_done_latch(6, Some((0, 1))),
                "done == 1",
                &[]
            )
            .unwrap(),
            PropertyVerdict::Violated
        );
        assert_eq!(
            verify_recoverability_scalable(
                &held_pair_done_latch(6, Some((0, 0))),
                "done == 1",
                &[]
            )
            .unwrap(),
            PropertyVerdict::Holds
        );
    }

    /// The safety cube path had the same hole: `AG ¬bad` claimed from the single pinned-to-0 initial
    /// state. A 2-bit held register with no `init` and `bad = (x == 3)`: `x = 3` IS an initial state.
    #[test]
    fn free_init_register_is_enumerated_in_the_safety_reset_cube() {
        let src = "1 sort bitvec 1\n2 sort bitvec 2\n3 state 2 x\n4 next 2 3 3\n5 constd 2 3\n6 eq 1 3 5\n7 bad 6\n";
        let v = verify_safety_scalable(src).expect("safety cube runs");
        assert_ne!(
            v,
            PropertyVerdict::Holds,
            "a free-init register pinned to 0 hid the reachable bad state"
        );
        assert_eq!(v, PropertyVerdict::Violated);
        // With `init x = 0` the bad state is unreachable: the pinned verdict is HOLDS.
        let pinned = "1 sort bitvec 1\n2 sort bitvec 2\n3 state 2 x\n4 next 2 3 3\n5 constd 2 3\n6 eq 1 3 5\n7 bad 6\n8 zero 2\n9 init 2 3 8\n";
        assert_eq!(
            verify_safety_scalable(pinned).unwrap(),
            PropertyVerdict::Holds
        );
    }

    #[test]
    fn reset_cube_verdict_trusts_free_init_flavours_both_ways_and_select_flavours_holds_only() {
        use crate::mu_calculus::Trit;
        // bit 0 pinned true; bit 1 free-init; bit 2 free-select. verdict_at reads the cube index.
        let table = |cube: usize| -> Trit {
            match cube {
                0b001 | 0b011 => Trit::True,
                0b101 => Trit::False,
                _ => Trit::Unknown,
            }
        };
        // free-init bit 1 only: flavours 0b001 (True) and 0b011 (True) → Holds.
        assert_eq!(
            reset_cube_verdict(table, 0b001, &[1], &[]),
            PropertyVerdict::Holds
        );
        // free-init over a table with a False flavour → Violated (a real initial state violates).
        let t2 = |cube: usize| {
            if cube == 0b011 {
                Trit::False
            } else {
                Trit::True
            }
        };
        assert_eq!(
            reset_cube_verdict(t2, 0b001, &[1], &[]),
            PropertyVerdict::Violated
        );
        // select bit 2 only: 0b001 True, 0b101 False → NOT Violated (over-approx), Unknown.
        assert_eq!(
            reset_cube_verdict(table, 0b001, &[], &[2]),
            PropertyVerdict::Unknown
        );
        // too many free bits → abstain.
        assert_eq!(
            reset_cube_verdict(table, 0, &[0, 1, 2], &[3, 4]),
            PropertyVerdict::Unknown
        );
    }

    // === P2 Slice 1 — the MANDATORY differential soundness gate ==================
    // The scalable cube + smt-hyper-must verdict MUST equal the exact-engine verdict
    // on the small fixtures the exact engine decides, in BOTH polarities. This is the
    // non-negotiable soundness assertion (the L5 differential-oracle rule).

    #[test]
    fn scalable_matches_exact_holds_polarity() {
        // Differential SOUNDNESS gate: the cube path agrees with the exact engine on RESPONDER
        // (both `Holds`). The N1-first-increment property-directed seeding (auto-seeding the good
        // register's other control states — `st==1`, `st==2`) splits the coarse `good`-vs-`!good`
        // abstraction so cube 1's must-self-loop resolves and `EF idle` is provably `Holds`. Before
        // the increment this abstained to `Unknown` (sound but imprecise). The gate stays soundness
        // (`agree or abstain, never contradict`) but is now met by AGREEMENT.
        let exact = exact_verdict(RESPONDER, "st == 0");
        let scalable = verify_recoverability_scalable(RESPONDER, "st == 0", &[]).expect("decides");
        assert_eq!(
            exact,
            PropertyVerdict::Holds,
            "exact decides RESPONDER Holds"
        );
        assert_eq!(
            scalable, exact,
            "cube path (with property-directed seeding) must DECIDE RESPONDER Holds, agreeing with exact"
        );
    }

    #[test]
    fn scalable_matches_exact_violated_polarity() {
        // STALLER: the reachable `stuck` state cannot get back to idle ⇒ Violated,
        // both engines.
        let exact = exact_verdict(STALLER, "st == 0");
        let scalable = verify_recoverability_scalable(STALLER, "st == 0", &[]).expect("decides");
        assert_eq!(
            exact,
            PropertyVerdict::Violated,
            "exact decides STALLER Violated"
        );
        assert_eq!(
            scalable, exact,
            "cube path must AGREE with the exact engine on STALLER (Violated)"
        );
    }

    // === The proof the slice decides AT SCALE ==================================
    // On a design whose cone exceeds the exact auto-cap ceiling (64), the exact engine
    // ABSTAINS (Unknown) while the cube path DECIDES — in both polarities.

    #[test]
    fn wide_design_exact_abstains_but_cube_decides_holds() {
        // The 80-bit counter feeds st's cone ⇒ the exact engine over-caps (Unknown),
        // but the cube abstraction (which drops the counter) decides Holds.
        assert_eq!(
            exact_verdict(WIDE_RECOVERABLE, "st == 0"),
            PropertyVerdict::Unknown,
            "the exact engine must ABSTAIN on the wide (over-cap) design"
        );
        assert_eq!(
            verify_recoverability_scalable(WIDE_RECOVERABLE, "st == 0", &[]).expect("decides"),
            PropertyVerdict::Holds,
            "the cube path must DECIDE Holds where the exact engine abstains"
        );
    }

    #[test]
    fn wide_design_exact_abstains_but_cube_decides_violated() {
        assert_eq!(
            exact_verdict(WIDE_TRAP, "st == 0"),
            PropertyVerdict::Unknown,
            "the exact engine must ABSTAIN on the wide (over-cap) trap design"
        );
        assert_eq!(
            verify_recoverability_scalable(WIDE_TRAP, "st == 0", &[]).expect("decides"),
            PropertyVerdict::Violated,
            "the cube path must DECIDE Violated where the exact engine abstains"
        );
    }

    // === SOUNDNESS regression (2026-07-11): UF-wrap must NOT manufacture a spurious `Holds` =====
    // `data' = data + 2`, `data` init 1 ⇒ `data` stays ODD ⇒ `data==0` is unreachable ⇒ the
    // `busy --(data==0)--> done` escape never fires ⇒ `busy` is an absorbing trap ⇒ `AG EF idle`
    // is VIOLATED. At width 8 (cone under the exact cap) the exact engine proves it. At width 48
    // the wide `add` (> UF_WIDE_ADD_SUB_THRESHOLD = 32) is UF-wrapped; the pre-fix cube path
    // reported an unsound `Holds` (may-havoc manufactured the `data==0` escape). The fix ABSTAINS.
    const TRAP_UF_W8: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 sort bitvec 8
4 state 1 ctrl
5 zero 1
6 init 1 4 5
7 state 3 data
8 zero 3
13 one 3
30 constd 3 2
9 init 3 7 13
10 input 2 start
11 one 1
12 constd 1 2
14 eq 2 4 5
15 eq 2 4 11
23 eq 2 7 8
17 ite 1 10 11 5
25 ite 1 23 12 11
18 ite 1 15 25 5
19 ite 1 14 17 18
20 next 1 4 19
31 add 3 7 30
22 next 3 7 31
";
    const TRAP_UF_W48: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 sort bitvec 48
4 state 1 ctrl
5 zero 1
6 init 1 4 5
7 state 3 data
8 zero 3
13 one 3
30 constd 3 2
9 init 3 7 13
10 input 2 start
11 one 1
12 constd 1 2
14 eq 2 4 5
15 eq 2 4 11
23 eq 2 7 8
17 ite 1 10 11 5
25 ite 1 23 12 11
18 ite 1 15 25 5
19 ite 1 14 17 18
20 next 1 4 19
31 add 3 7 30
22 next 3 7 31
";

    // Wide (48-bit) datapath + control-return recoverability. `ctrl` idle(0)→busy(1)→done(2)→idle,
    // return is DATA-INDEPENDENT ⇒ `AG EF (ctrl==0)` HOLDS regardless of `data`. `data` is in `ctrl`'s
    // cone (idle→busy gated on `data==0`) so the exact engine over-caps; the wide op UF-wraps.
    const POS_W48: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 sort bitvec 48
4 state 1 ctrl
5 zero 1
6 init 1 4 5
7 state 3 data
8 zero 3
9 init 3 7 8
10 input 2 start
11 one 1
12 constd 1 2
13 one 3
14 eq 2 4 5
15 eq 2 4 11
23 eq 2 7 8
24 and 2 10 23
17 ite 1 24 11 5
18 ite 1 15 12 5
19 ite 1 14 17 18
20 next 1 4 19
21 add 3 7 13
22 next 3 7 21
";
    // Same control FSM, but `data' = data * data` — a 48-bit MULTIPLIER the exact BDD cannot build.
    const MULT_W48: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 sort bitvec 48
4 state 1 ctrl
5 zero 1
6 init 1 4 5
7 state 3 data
8 zero 3
9 init 3 7 8
10 input 2 start
11 one 1
12 constd 1 2
13 one 3
14 eq 2 4 5
15 eq 2 4 11
23 eq 2 7 8
24 and 2 10 23
17 ite 1 24 11 5
18 ite 1 15 12 5
19 ite 1 14 17 18
20 next 1 4 19
26 mul 3 7 7
22 next 3 7 26
";

    #[test]
    fn uf_wrap_recoverability_sound_and_decides_control_return() {
        // Ground truth (exact, width-8 cone under the cap): the odd-counter trap is VIOLATED.
        assert_eq!(
            exact_verdict(TRAP_UF_W8, "ctrl == 0"),
            PropertyVerdict::Violated,
            "exact engine (under cap) must prove the odd-counter trap VIOLATED"
        );

        // SOUNDNESS (PR #302 universal-◇): the wrapped trap is NEVER a spurious `Holds`. The auto
        // path now RUNS the wrapped cube (guard relaxed) and lands on a sound `⊥` — the coarse
        // abstraction can't prove the trap — but the universal ◇ forbids fabricating `Holds`.
        let trap = verify_recoverability_scalable(TRAP_UF_W48, "ctrl == 0", &[]).expect("decides");
        assert_ne!(
            trap,
            PropertyVerdict::Holds,
            "the wrapped trap must NEVER be a spurious Holds (got {trap:?})"
        );

        // PERF GUARD: a CALLER-supplied predicate on a wrapped design abstains (it may force the wide
        // op into the cube successor → the all-pairs-SMT cost). Empty extras (the auto path) runs.
        let extra = vec![parse_extra_predicate("dz:data=0").expect("parse")];
        assert_eq!(
            verify_recoverability_scalable(TRAP_UF_W48, "ctrl == 0", &extra).expect("abstains"),
            PropertyVerdict::Unknown,
            "a caller-supplied predicate on a wrapped design abstains (perf guard)"
        );

        // SCALE WIN — the N1 first increment (property-directed control-state seeding) DECIDES the
        // wide-datapath control-return class the exact BDD walls on. `data` is may-side (UF-wrapped,
        // sound over-approx); the seeded control predicates + exact-transition must decide the
        // datapath-independent return. Both `Holds`, including a 48-bit MULTIPLIER (`data*data`) whose
        // relation the exact BDD cannot even build.
        assert_eq!(
            verify_recoverability_scalable(POS_W48, "ctrl == 0", &[]).expect("decides"),
            PropertyVerdict::Holds,
            "wide-add control-return recoverability decides Holds"
        );
        assert_eq!(
            verify_recoverability_scalable(MULT_W48, "ctrl == 0", &[]).expect("decides"),
            PropertyVerdict::Holds,
            "48-bit MULTIPLIER control-return recoverability decides Holds (exact BDD walls)"
        );
    }

    // Class-2: datapath-DEPENDENT return. `busy → idle` only when `data == 7` (K=7, a 48-bit design
    // literal); `data == 7` is invariant (data' = data, init 7). `AG EF (ctrl==0)` HOLDS but needs the
    // `data == 7` predicate: control-state seeding alone (good register only) leaves it `⊥`, and K=7
    // cannot be enumerated (2^48 values). The guard-atom extraction reads `data == 7` off the design's
    // own comparison node and decides it.
    const CLASS2_DATADEP: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 sort bitvec 48
4 state 1 ctrl
5 zero 1
6 init 1 4 5
7 state 3 data
30 constd 3 7
9 init 3 7 30
10 input 2 start
11 one 1
14 eq 2 4 5
15 eq 2 4 11
23 eq 2 7 30
17 ite 1 10 11 5
25 ite 1 23 5 11
18 ite 1 15 25 5
19 ite 1 14 17 18
20 next 1 4 19
22 next 3 7 7
";

    #[test]
    fn class2_datapath_dependent_return_decides_via_guard_atoms() {
        // Class-2 discovery: the return reads a datapath predicate (`data == 7`) that control-state
        // seeding does not cover. The guard-atom extraction discovers it from the design's `eq`
        // comparison node (K=7, a 48-bit literal, unenumerable over `2^W`), and the datapath-dependent
        // recoverability decides `Holds`. (Diagnostic: `data==0`/`data==1` — what value-enumeration
        // would propose — leave it `⊥`; only `data==7`, the guard atom, decides.)
        assert_eq!(
            verify_recoverability_scalable(CLASS2_DATADEP, "ctrl == 0", &[]).expect("decides"),
            PropertyVerdict::Holds,
            "datapath-dependent-return recoverability decides Holds via guard-atom discovery"
        );
    }

    // Class-2 RELATIONAL return. `busy → idle` (`ctrl' = ctrl & !(data==target)`) fires iff the two
    // 48-bit registers are equal; both init 0 and increment by 1, so `data == target` is INVARIANT →
    // `AG EF (ctrl==0)` HOLDS. But no `register == constant` predicate captures it: the guard compares
    // two STATE registers, and the constant K is not enumerable. `eq_reg_guard_atoms` reads the
    // relational atom `data == target` off the design's `eq` node; the exact hyper-must edge preserves
    // the invariant across the wide (UF-wrapped-on-the-may-side) `+1` increments WITHOUT concretising
    // the values, so the return decides at scale where the exact BDD walls.
    fn relational_w(width: u32, target_init_nid: &str) -> String {
        format!(
            "1 sort bitvec 1\n2 sort bitvec {width}\n3 state 1 ctrl\n4 state 2 data\n\
             5 state 2 target\n6 zero 2\n7 one 2\n9 one 1\n10 init 1 3 9\n11 init 2 4 6\n\
             12 init 2 5 {target_init_nid}\n13 add 2 4 7\n14 add 2 5 7\n15 eq 1 4 5\n16 not 1 15\n\
             17 and 1 3 16\n18 next 1 3 17\n19 next 2 4 13\n20 next 2 5 14\n"
        )
    }

    #[test]
    fn class2_relational_return_decides_via_reg_eq_reg_atoms() {
        // POSITIVE (target init 0 == data init 0): `data == target` invariant → recoverable.
        let pos = relational_w(48, "6"); // nid 6 = zero
        assert_eq!(
            verify_recoverability_scalable(&pos, "ctrl == 0", &[]).expect("decides"),
            PropertyVerdict::Holds,
            "48-bit RELATIONAL-return recoverability decides Holds via reg==reg atom discovery"
        );
        // NEGATIVE (target init 1 != data init 0): `data == target` NEVER holds → `busy` is a trap →
        // NOT recoverable. The relational predicate soundly reports Violated (not a spurious Holds).
        let neg = relational_w(48, "7"); // nid 7 = one
        assert_eq!(
            verify_recoverability_scalable(&neg, "ctrl == 0", &[]).expect("decides"),
            PropertyVerdict::Violated,
            "the never-equal relational trap decides Violated"
        );

        // DIFFERENTIAL ORACLE: the same designs at 8-bit route through the EXACT BDD engine (below the
        // 40-bit cap) via the public verb. The narrow exact verdict must equal the wide cube verdict —
        // zero mismatches is the soundness gate for the relational seeding.
        assert_eq!(
            verify_recoverability(&relational_w(8, "6"), "ctrl == 0").expect("exact decides"),
            PropertyVerdict::Holds,
            "8-bit exact oracle must agree with the 48-bit cube Holds"
        );
        assert_eq!(
            verify_recoverability(&relational_w(8, "7"), "ctrl == 0").expect("exact decides"),
            PropertyVerdict::Violated,
            "8-bit exact oracle must agree with the 48-bit cube Violated"
        );
    }

    // Frontier (b′) — arithmetic-relational return `data == target + 2` (`ctrl' = ctrl & !(data==target+2)`).
    // Both registers increment, so `data == target + 2` is INVARIANT when `data` inits `target + 2`. The
    // addend form is neither a literal K nor a bare `reg == reg` guard, so the earlier extractions miss
    // it; `eq_reg_addend_guard_atoms` reads it off `eq(data, add(target, 2))` and seeds the
    // `CmpRegAddend`, decided at scale by the exact must edge preserving the invariant across the wide
    // increments (mod-2^width).
    fn arith_rel_w(width: u32, data_init: u64) -> String {
        format!(
            "1 sort bitvec 1\n2 sort bitvec {width}\n3 state 1 ctrl\n4 state 2 data\n\
             5 state 2 target\n6 zero 1\n7 one 1\n8 one 2\n9 constd 2 2\n10 constd 2 {data_init}\n\
             11 constd 2 3\n12 init 1 3 7\n13 init 2 4 10\n14 init 2 5 11\n15 add 2 4 8\n\
             16 add 2 5 8\n17 add 2 5 9\n18 eq 1 4 17\n19 not 1 18\n20 and 1 3 19\n21 next 1 3 20\n\
             22 next 2 4 15\n23 next 2 5 16\n"
        )
    }

    #[test]
    fn class2_frontier_bprime_arith_relational_return_decides_via_addend_atoms() {
        // POSITIVE (data init 5 == target(3)+2): `data == target + 2` invariant → recoverable.
        assert_eq!(
            verify_recoverability_scalable(&arith_rel_w(48, 5), "ctrl == 0", &[]).expect("decides"),
            PropertyVerdict::Holds,
            "48-bit arithmetic-relational return decides Holds via addend-atom discovery"
        );
        // NEGATIVE (data init 2 != target(3)+2): the relation never holds → busy is a trap → Violated.
        assert_eq!(
            verify_recoverability_scalable(&arith_rel_w(48, 2), "ctrl == 0", &[]).expect("decides"),
            PropertyVerdict::Violated,
            "the never-equal arithmetic-relational trap decides Violated"
        );
        // DIFFERENTIAL ORACLE at 8-bit (exact BDD) — must agree with the wide cube verdicts.
        assert_eq!(
            verify_recoverability(&arith_rel_w(8, 5), "ctrl == 0").expect("exact decides"),
            PropertyVerdict::Holds,
            "8-bit exact oracle must agree with the 48-bit cube Holds"
        );
        assert_eq!(
            verify_recoverability(&arith_rel_w(8, 2), "ctrl == 0").expect("exact decides"),
            PropertyVerdict::Violated,
            "8-bit exact oracle must agree with the 48-bit cube Violated"
        );
    }

    // Frontier (a) — COI-directed seeding. A 2-state ctrl FSM whose return is gated on `data == 42`
    // (data'=data → invariant when `data` inits 42), PLUS a decoy register `mode` compared against
    // constants 2..9 in dead logic (`mode` is NOT in ctrl's cone-of-influence). The UN-directed eager
    // extraction seeds `good == v` for every global constant (2..9, 42) and every `eq` atom
    // (`mode==2..9`), inflating the cube to a >30s all-pairs-SMT hang. COI-directed seeding restricts
    // both the control-state constant pool and the guard atoms to ctrl's cone → seeds only `data == 42`
    // (+ good, + ctrl==1) → decides fast. `AG EF (ctrl==0)` HOLDS.
    fn manyguard_btor2(width: u32, data_init: u64) -> String {
        let mut l: Vec<String> = vec![
            "1 sort bitvec 4".into(),
            "2 sort bitvec 1".into(),
            format!("3 sort bitvec {width}"),
            "4 state 1 ctrl".into(),
            "5 state 3 data".into(),
            "6 state 1 mode".into(),
            "10 zero 1".into(),
            "11 one 1".into(),
            "12 constd 3 42".into(),            // K
            format!("13 constd 3 {data_init}"), // data reset value
        ];
        let mut dc: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
        let mut nid = 20i64;
        for k in 2..10 {
            l.push(format!("{nid} constd 1 {k}"));
            dc.insert(k, nid);
            nid += 1;
        }
        l.push("14 init 1 4 11".into()); // ctrl = busy(1)
        l.push("15 init 3 5 13".into()); // data = data_init
        l.push(format!("16 init 1 6 {}", dc[&2])); // mode = 2
        // return: busy && data==42
        l.push(format!("{nid} eq 2 4 11"));
        let busy = nid;
        nid += 1;
        l.push(format!("{nid} eq 2 5 12"));
        let dk = nid;
        nid += 1;
        l.push(format!("{nid} and 2 {busy} {dk}"));
        let ret = nid;
        nid += 1;
        l.push(format!("{nid} ite 1 {ret} 10 4")); // busy&&data==42 -> idle(0) else stay
        let cn = nid;
        nid += 1;
        l.push(format!("{nid} next 1 4 {cn}"));
        nid += 1;
        l.push(format!("{nid} next 3 5 5")); // data' = data
        nid += 1;
        // decoy `mode` ring over eq(mode, 2..9) — dead logic, not in ctrl's cone
        let mut chain = 6i64;
        for k in 2..10 {
            let nxt = *dc.get(&(k + 1)).unwrap_or(&dc[&2]);
            l.push(format!("{nid} eq 2 6 {}", dc[&k]));
            let eqk = nid;
            nid += 1;
            l.push(format!("{nid} ite 1 {eqk} {nxt} {chain}"));
            chain = nid;
            nid += 1;
        }
        l.push(format!("{nid} next 1 6 {chain}"));
        l.join("\n")
    }

    #[test]
    fn class2_frontier_a_coi_directed_seeding_scales_past_decoy_guards() {
        // POSITIVE (data init 42): decides Holds fast — COI-direction drops the decoy `mode` guards and
        // the decoy control constants, so |P| stays small where the un-directed path blows up (>30s).
        assert_eq!(
            verify_recoverability_scalable(&manyguard_btor2(48, 42), "ctrl == 0", &[])
                .expect("decides"),
            PropertyVerdict::Holds,
            "COI-directed seeding decides the many-decoy-guard design (un-directed blows up)"
        );
        // NEGATIVE (data init 7 != 42): `data == 42` never holds → busy is a trap → not recoverable.
        assert_eq!(
            verify_recoverability_scalable(&manyguard_btor2(48, 7), "ctrl == 0", &[])
                .expect("decides"),
            PropertyVerdict::Violated,
            "the never-return many-guard variant decides Violated"
        );
        // DIFFERENTIAL ORACLE at 8-bit (exact BDD) — must agree with the wide cube verdicts.
        assert_eq!(
            verify_recoverability(&manyguard_btor2(8, 42), "ctrl == 0").expect("exact decides"),
            PropertyVerdict::Holds,
            "8-bit exact oracle must agree with the 48-bit cube Holds"
        );
        assert_eq!(
            verify_recoverability(&manyguard_btor2(8, 7), "ctrl == 0").expect("exact decides"),
            PropertyVerdict::Violated,
            "8-bit exact oracle must agree with the 48-bit cube Violated"
        );
    }

    // N1 EMERGENT-K — a control return decided by INEQUALITIES (`data >= target && target >= data`),
    // so the design carries the deciding relation `data == target` with NO `eq`/`add` node for the
    // syntactic frontiers (b)/(b′) to lift. The relation is therefore EMERGENT: only
    // `discover_inductive_relational_invariants` recovers it, by checking over the EXACT transition that
    // the register-pair difference is invariant, and seeding it — so the wide cube decides where it
    // otherwise abstains at ⊥. (Node 6 = zero, node 7 = one, used as the two `target` reset values.)
    fn emergent_ineq_w(width: u32, target_init_nid: &str) -> String {
        format!(
            "1 sort bitvec 1\n2 sort bitvec {width}\n3 state 1 ctrl\n4 state 2 data\n5 state 2 target\n\
             6 zero 2\n7 one 2\n9 one 1\n10 init 1 3 9\n11 init 2 4 6\n12 init 2 5 {target_init_nid}\n\
             13 add 2 4 7\n14 add 2 5 7\n15 ugte 1 4 5\n16 ugte 1 5 4\n17 and 1 15 16\n18 not 1 17\n\
             19 and 1 3 18\n20 next 1 3 19\n21 next 2 4 13\n22 next 2 5 14\n"
        )
    }

    #[test]
    fn emergent_k_discovers_inductive_relation_where_no_node_exists() {
        // NEGATIVE (target init 1 = node 7; data init 0): `target == data + 1` is INVARIANT, so
        // `data == target` (idle) is NEVER reached → busy is a trap → `AG EF (ctrl==0)` VIOLATED. Idle is
        // decided by two `ugte`s (no `eq` node), so the syntactic seeders find nothing and the wide cube
        // abstains at ⊥; the difference-invariant discovery recovers `target == data + 1` and refutes the
        // spurious havoc-equal states.
        assert_eq!(
            verify_recoverability_scalable(&emergent_ineq_w(48, "7"), "ctrl == 0", &[])
                .expect("decides"),
            PropertyVerdict::Violated,
            "48-bit emergent (no-eq-node) trap decides Violated via inductive relational-invariant discovery"
        );
        // POSITIVE (target init 0 = node 6 == data): `data == target` invariant → recoverable → Holds.
        assert_eq!(
            verify_recoverability_scalable(&emergent_ineq_w(48, "6"), "ctrl == 0", &[])
                .expect("decides"),
            PropertyVerdict::Holds,
            "48-bit emergent positive decides Holds (discovery does not regress the recoverable case)"
        );
        // DIFFERENTIAL ORACLE at 8-bit (exact BDD) — must agree with the wide cube verdicts.
        assert_eq!(
            verify_recoverability(&emergent_ineq_w(8, "7"), "ctrl == 0").expect("exact decides"),
            PropertyVerdict::Violated,
            "8-bit exact oracle agrees with the 48-bit emergent Violated"
        );
        assert_eq!(
            verify_recoverability(&emergent_ineq_w(8, "6"), "ctrl == 0").expect("exact decides"),
            PropertyVerdict::Holds,
            "8-bit exact oracle agrees with the 48-bit emergent Holds"
        );
    }

    // Relational recoverability TARGET — `AG EF (data == target)` where the good atom itself is a
    // register-vs-register relation, not `REG == VALUE`. data,target both init 0 and increment, so the
    // relation is INVARIANT and the property Holds; the target flows through the compound-good machinery
    // (the reset cube evaluates the relation). Decides at 48-bit where the exact engine walls. (Contrast:
    // a FIFO's `AG EF (wptr == rptr)` reaches the relation by DRAINING — independent counters — which is
    // the ranking class and correctly abstains at scale; this test covers the invariant case the cube
    // decides.)
    fn inv_rel_w(width: u32, target_init_nid: &str) -> String {
        format!(
            "1 sort bitvec 1\n2 sort bitvec {width}\n4 state 2 data\n5 state 2 target\n6 zero 2\n\
             7 one 2\n13 init 2 4 6\n14 init 2 5 {target_init_nid}\n15 add 2 4 7\n16 add 2 5 7\n\
             22 next 2 4 15\n23 next 2 5 16\n"
        )
    }

    #[test]
    fn relational_recoverability_target_decides_at_scale() {
        // POSITIVE (target inits 0 == data): `data == target` invariant → Holds at 48-bit via the
        // relational target, where exact walls.
        assert_eq!(
            verify_recoverability(&inv_rel_w(48, "6"), "data == target").expect("decides"),
            PropertyVerdict::Holds,
            "48-bit invariant-relational recoverability target decides Holds"
        );
        // NEGATIVE (target inits 1 != data): never equal → Violated.
        assert_eq!(
            verify_recoverability(&inv_rel_w(48, "7"), "data == target").expect("decides"),
            PropertyVerdict::Violated,
            "the never-equal relational target decides Violated"
        );
        // DIFFERENTIAL ORACLE at 8-bit — must agree with the 48-bit verdicts.
        assert_eq!(
            verify_recoverability(&inv_rel_w(8, "6"), "data == target").expect("small decides"),
            PropertyVerdict::Holds
        );
        assert_eq!(
            verify_recoverability(&inv_rel_w(8, "7"), "data == target").expect("small decides"),
            PropertyVerdict::Violated
        );
    }

    /// N1 boolean-gated-event rewrite — a `good` EVENT `done == 1` that is driven EXACTLY by a
    /// `counter == threshold` gate (`done <= (cnt == 0)`) reduces to the counter recoverability
    /// `AG EF (cnt == 0)`, decided by the ranking (the boolean `done` carries no measure; the cube
    /// over `{done == 1}` alone cannot track `cnt`). A GATED driver (`done <= enable ? (cnt==0) : 0`)
    /// is NOT a pure `Eq`, so the rewrite abstains (soundness: `done == 1` is not equivalent to
    /// `cnt == 0`).
    #[test]
    fn boolean_gated_event_rewrite_decides_via_counter_ranking() {
        // done <= (cnt == 0); cnt a down-counter to 0 (`cnt' = ite(cnt==0, 0, cnt-1)`). Pure Eq driver
        // (node 8). Mirrors the passing `downcnt_w` fixture (decimal `constd`, `zero`/`one`).
        let clean = "1 sort bitvec 1\n2 sort bitvec 8\n3 state 2 cnt\n4 zero 2\n5 one 2\n\
             6 constd 2 5\n7 init 2 3 6\n8 eq 1 3 4\n9 sub 2 3 5\n10 ite 2 8 4 9\n11 next 2 3 10\n\
             12 state 1 done\n13 zero 1\n14 init 1 12 13\n15 next 1 12 8\n";
        let file = crate::adapter::btor2::parser::parse(clean).expect("parse clean");
        assert_eq!(
            counter_gate_of(&file, "done"),
            Some(("cnt".to_string(), 0)),
            "the pure `done <= (cnt == 0)` driver rewrites to the counter gate (cnt, 0)"
        );
        // DIAGNOSTIC: the counter recoverability itself + the ranking directly.
        assert!(
            ranking_certificate_holds(&file, &["cnt".to_string()], Some(0)),
            "the ranking certifies the down-counter cnt descends to 0"
        );
        assert_eq!(
            verify_recoverability_scalable(clean, "cnt == 0", &[]).expect("decides"),
            PropertyVerdict::Holds,
            "the counter recoverability AG EF (cnt==0) decides Holds"
        );
        assert_eq!(
            verify_recoverability_scalable(clean, "done == 1", &[]).expect("decides"),
            PropertyVerdict::Holds,
            "boolean-gated event AG EF (done==1) decides Holds via the counter-gate ranking rewrite"
        );
        // GATED: done <= enable ? (cnt==0) : 0 — an `Ite` driver (node 17), not a bare `Eq` ⇒ abstain.
        let gated = "1 sort bitvec 1\n2 sort bitvec 8\n3 state 2 cnt\n4 zero 2\n5 one 2\n\
             6 constd 2 5\n7 init 2 3 6\n8 eq 1 3 4\n9 sub 2 3 5\n10 ite 2 8 4 9\n11 next 2 3 10\n\
             12 state 1 done\n13 zero 1\n14 init 1 12 13\n16 input 1 enable\n17 ite 1 16 8 13\n\
             18 next 1 12 17\n";
        let gfile = crate::adapter::btor2::parser::parse(gated).expect("parse gated");
        assert_eq!(
            counter_gate_of(&gfile, "done"),
            None,
            "a gated (non-pure-Eq) event driver is not rewritten (soundness)"
        );
    }

    /// Actionable-⊥ diagnosis — a recovery target whose value rides a WIDE datapath register. The
    /// diagnosis must localize the 16-bit `wide_data` influence (recovery value is data-scoped) and
    /// find no gating counter.
    #[test]
    fn recoverability_bot_diagnosis_flags_wide_data_influence() {
        const D: &str = "1 sort bitvec 1\n2 sort bitvec 16\n3 state 1 flag\n4 state 2 wide_data\n\
5 zero 1\n6 one 1\n7 zero 2\n8 one 2\n9 init 1 3 5\n10 init 2 4 7\n11 slice 1 4 0 0\n\
12 next 1 3 11\n13 add 2 4 8\n14 next 2 4 13\n";
        let diag = diagnose_recoverability_bot(D, "flag == 1");
        assert!(
            diag.uncertified_counters.is_empty(),
            "no down-counter gates it"
        );
        assert_eq!(
            diag.wide_influences,
            vec![("wide_data".to_string(), 16)],
            "the 16-bit datapath the recovery value rides is localized"
        );
        assert!(diag.hint().contains("wide data/config state"));
        assert!(!diag.is_empty());
    }

    /// Refined verdict (Phase 0) — a HOLDS design carries an EMPTY refinement: the target is
    /// recoverable (so not vacuous) and the verdict is definite (so no ⊥ hint). `flag` toggles, so
    /// `AG EF(flag==0)` HOLDS.
    #[test]
    fn refined_verdict_holds_design_has_empty_refinement() {
        const TOGGLE: &str =
            "1 sort bitvec 1\n2 state 1 flag\n3 zero 1\n4 init 1 2 3\n5 not 1 2\n6 next 1 2 5\n";
        let (verdict, refinement) =
            verify_recoverability_refined(TOGGLE, "flag == 0", &[], &[], false);
        assert_eq!(verdict, PropertyVerdict::Holds);
        assert!(
            refinement.is_empty(),
            "a definite, recoverable HOLDS needs no refinement: {refinement:?}"
        );
    }

    /// Refined verdict (Phase 0) — a design whose target is NEVER reachable is flagged `Vacuous`:
    /// `flag` is init 0 and stuck at 0 (`next = 0`), so `flag == 1` is unreachable and `AG EF(flag==1)`
    /// is degenerate. The canonical verdict is NOT Holds; the refinement carries the vacuity witness.
    #[test]
    fn refined_verdict_vacuous_target_is_flagged() {
        const STUCK: &str =
            "1 sort bitvec 1\n2 state 1 flag\n3 zero 1\n4 init 1 2 3\n5 next 1 2 3\n";
        let (verdict, refinement) =
            verify_recoverability_refined(STUCK, "flag == 1", &[], &[], false);
        assert_ne!(
            verdict,
            PropertyVerdict::Holds,
            "a stuck-at-0 flag can never recover to 1"
        );
        let vac = refinement
            .vacuous
            .expect("an unreachable target must be flagged Vacuous");
        assert!(vac.good_unreachable);
        assert!(vac.note.contains("never reachable"));
    }

    // `busy` sets on `start`; it then recovers to 0 UNLESS `mode == 3`, in which case it is stuck at 1.
    // So `AG EF(busy==0)` HOLDS for mode ∈ {0,1,2} and is VIOLATED for mode == 3 — config-dependent.
    const MODE_DEP: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 input 1 start
4 input 2 mode
5 state 1 busy
6 zero 1
7 init 1 5 6
8 const 2 11
9 eq 1 4 8
10 not 1 5
11 and 1 3 10
12 and 1 5 9
13 or 1 11 12
14 next 1 5 13
";

    /// Config-partition (Phase 1) — the verdict genuinely DEPENDS on the `mode` config input, so a
    /// flat verdict becomes `ConfigDependent`: HOLDS for `mode ∈ {0,1,2}`, VIOLATED for `mode == 3`.
    #[test]
    fn config_partition_reveals_mode_dependent_recoverability() {
        let part = config_partition(
            MODE_DEP,
            "busy == 0",
            &[("mode".to_string(), vec![0, 1, 2, 3])],
        )
        .expect("the property depends on `mode` ⇒ a ConfigDependent partition");
        assert_eq!(part.config_atoms, vec![("mode".to_string(), 2)]);
        assert!(
            part.exhaustive,
            "all 4 values of the 2-bit `mode` were enumerated"
        );
        assert_eq!(
            part.holds.len(),
            3,
            "mode ∈ {{0,1,2}} HOLD: {:?}",
            part.holds
        );
        assert_eq!(
            part.violated,
            vec![vec![("mode".to_string(), 3)]],
            "only mode == 3 traps"
        );
        assert!(part.unknown.is_empty() && part.vacuous.is_empty());
    }

    /// Config-partition — a design whose recovery does NOT depend on `mode` (`busy` always recovers):
    /// an EXPLICIT spec is still reported as asked (every named value holds — the scoped answer), and
    /// [`depends_on_config`] is what says the partition carries nothing the bare verdict did not
    /// (the auto axis collapses on it; mununu#634 stopped the explicit path from doing the same).
    #[test]
    fn config_partition_reports_an_explicit_config_independent_spec_as_all_holds() {
        const INDEP: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 input 1 start
4 input 2 mode
5 state 1 busy
6 zero 1
7 init 1 5 6
8 not 1 5
9 and 1 3 8
10 next 1 5 9
";
        let part = config_partition(
            INDEP,
            "busy == 0",
            &[("mode".to_string(), vec![0, 1, 2, 3])],
        )
        .expect("an explicit spec is reported as asked");
        assert_eq!(part.holds.len(), 4, "{part:?}");
        assert!(part.violated.is_empty() && part.unknown.is_empty());
        assert!(
            !depends_on_config(&part),
            "recovery independent of `mode`: one non-empty cell"
        );
    }

    /// mununu#634 — `--config-values mode=3`, ONE value: the scoped verdict the user asked for ("under
    /// this pin the property is violated"), reported as a one-cell partition. It used to collapse to
    /// `None` as "config-independent", so the pinned run showed an empty refinement and nothing for
    /// `--discover-assumptions` to key off.
    #[test]
    fn x634_an_explicit_single_value_pin_is_reported_as_a_one_cell_partition() {
        let part = config_partition(MODE_DEP, "busy == 0", &[("mode".to_string(), vec![3])])
            .expect("a one-value explicit spec is a scoped verdict, not config-independence");
        assert_eq!(
            part.violated,
            vec![vec![("mode".to_string(), 3)]],
            "mode == 3 traps: {part:?}"
        );
        assert!(part.holds.is_empty() && part.unknown.is_empty() && part.vacuous.is_empty());
        assert!(!part.exhaustive, "one of four values enumerated");
        assert!(!depends_on_config(&part));
        // The refined verb composes it: the canonical verdict is unchanged and the one cell rides along.
        let (verdict, refinement) = verify_recoverability_refined(
            MODE_DEP,
            "busy == 0",
            &[],
            &[("mode".to_string(), vec![3])],
            false,
        );
        assert_ne!(verdict, PropertyVerdict::Skipped);
        let part = refinement
            .config_partition
            .expect("the refinement carries the pinned cell");
        assert_eq!(part.violated, vec![vec![("mode".to_string(), 3)]]);
    }

    // `busy` sets on `start` and TRAPS at 1 (operational logic `busy | (start & !busy)` never clears);
    // the active-low reset `rst_n` (mux `ite(rst_n, op, 0)`) clears it. So held-in-reset (rst_n=0)
    // `AG EF(busy==0)` HOLDS; operationally (rst_n=1) it is VIOLATED — the branching pair the auto
    // reset-partition surfaces WITHOUT the user naming `rst_n`.
    const RESET_DEP: &str = "\
1 sort bitvec 1
2 input 1 rst_n
3 input 1 start
4 state 1 busy
5 zero 1
6 init 1 4 5
7 not 1 4
8 and 1 3 7
9 or 1 4 8
10 ite 1 2 9 5
11 next 1 4 10
";

    /// Slice 2b — AUTO config identification. With NO user-named config, the design's detected reset
    /// (`rst_n`) is the config axis: `auto_config_partition` partitions `AG EF(busy==0)` over {reset,
    /// operational} → `ConfigDependent{holds:[rst_n=0], violated:[rst_n=1]}` (the OpenTitan pattern,
    /// reproduced synthetically).
    #[test]
    fn auto_config_partition_reveals_reset_dependent_recoverability() {
        let part = auto_config_partition(RESET_DEP, "busy == 0")
            .expect("recovery depends on the detected reset ⇒ a ConfigDependent partition");
        assert_eq!(
            part.holds,
            vec![vec![("rst_n".to_string(), 0)]],
            "held-in-reset (rst_n=0) recovers"
        );
        assert_eq!(
            part.violated,
            vec![vec![("rst_n".to_string(), 1)]],
            "operationally (rst_n=1) `busy` traps"
        );
        assert!(part.unknown.is_empty() && part.vacuous.is_empty());
    }

    /// Slice 2b control — a design whose recovery does NOT depend on its reset (`busy` always clears)
    /// yields NO auto partition: reset-independent, the bare verdict already says it.
    #[test]
    fn auto_config_partition_none_when_reset_independent() {
        const RESET_INDEP: &str = "\
1 sort bitvec 1
2 input 1 rst_n
3 input 1 start
4 state 1 busy
5 zero 1
6 init 1 4 5
7 not 1 4
8 and 1 3 7
9 ite 1 2 8 5
10 next 1 4 9
";
        assert!(
            auto_config_partition(RESET_INDEP, "busy == 0").is_none(),
            "recovery independent of the reset ⇒ no auto partition"
        );
    }

    /// Slice 2b control — no reset input, no reset mux ⇒ nothing to auto-identify. (The `mode`-gated
    /// MODE_DEP IS config-dependent, but on a config REGISTER auto cannot name; the user must pass
    /// `--config-values mode=…`. This asserts auto stays silent rather than guessing.)
    #[test]
    fn auto_config_partition_none_without_a_reset() {
        assert!(
            auto_config_partition(MODE_DEP, "busy == 0").is_none(),
            "no reset detected ⇒ auto abstains (mode is a config register, named via --config-values)"
        );
    }

    /// Slice 2b end-to-end — bare `--refine` (empty `config_specs`) auto-populates `config_partition`
    /// from the detected reset. The canonical verdict stays HOLDS (the free reset can always recover),
    /// while the refinement reveals the operational trap.
    #[test]
    fn verify_recoverability_refined_auto_populates_config_partition_from_reset() {
        let (verdict, refinement) =
            verify_recoverability_refined(RESET_DEP, "busy == 0", &[], &[], false);
        assert_eq!(
            verdict,
            PropertyVerdict::Holds,
            "free reset ⇒ EF(busy==0) from every state ⇒ AG EF HOLDS"
        );
        let part = refinement
            .config_partition
            .expect("bare --refine auto-identifies the reset config axis");
        assert_eq!(part.holds, vec![vec![("rst_n".to_string(), 0)]]);
        assert_eq!(part.violated, vec![vec![("rst_n".to_string(), 1)]]);
    }

    // A trap FSM: IDLE(0) --go--> WORK(1); WORK --en=0--> FAULT(2, absorbing) / --en=1--> IDLE.
    // Free-input: FAULT is reachable (IDLE→WORK→en=0) and absorbing ⇒ `AG EF(st==0)` VIOLATED. Held
    // `en==1`: WORK always returns to IDLE and FAULT becomes UNREACHABLE ⇒ AG (over reachable states)
    // HOLDS — the non-monotone flip that assumption discovery surfaces as `HoldsUnder(en==1)`. Holding
    // `go==0` also makes it "HOLD" but VACUOUSLY (st never leaves 0) ⇒ the non-vacuity gate must reject it.
    const EN_TRAP: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 input 2 en
4 input 2 go
5 state 1 st
6 zero 1
7 init 1 5 6
8 one 1
9 constd 1 2
10 eq 2 5 6
11 eq 2 5 8
13 ite 1 4 8 6
14 ite 1 3 6 9
15 ite 1 11 14 9
16 ite 1 10 13 15
17 next 1 5 16
";

    /// Capability B slice 1 — assumption discovery finds the single-input hold `en == 1` under which the
    /// free-input-VIOLATED `AG EF(st==0)` becomes a NON-VACUOUS HOLDS (holding `en` high keeps the FAULT
    /// trap unreachable). The vacuous `go == 0` hold (st never leaves IDLE) must be REJECTED by the gate.
    #[test]
    fn discover_assumptions_finds_nonvacuous_input_hold() {
        let found = discover_assumptions(EN_TRAP, "st == 0", &[]);
        assert!(
            found.iter().any(|a| a.phi == "en == 1"
                && a.kind == AssumptionKind::InputHold
                && a.non_vacuous),
            "expected a non-vacuous HoldsUnder(en == 1): {found:?}"
        );
        assert!(
            !found.iter().any(|a| a.phi == "go == 0"),
            "the vacuous `go==0` hold (st stuck at IDLE) must fail the non-vacuity gate: {found:?}"
        );
    }

    /// Capability B slice 1 — end-to-end via `verify_recoverability_refined` (opt-in flag): the
    /// discovered assumption rides in `holds_under`, and the CANONICAL verdict stays VIOLATED (the
    /// conditional-only guard — assumptions are not monotone for `AG EF`, so a HoldsUnder never lifts
    /// the verdict to a bare Holds).
    #[test]
    fn refined_verdict_carries_discovered_assumption_without_changing_verdict() {
        let (verdict, refinement) =
            verify_recoverability_refined(EN_TRAP, "st == 0", &[], &[], true);
        assert_eq!(
            verdict,
            PropertyVerdict::Violated,
            "free-input the FAULT trap is reachable ⇒ canonical verdict stays VIOLATED"
        );
        assert!(
            refinement
                .holds_under
                .iter()
                .any(|a| a.phi == "en == 1" && a.non_vacuous),
            "the enabling assumption rides in holds_under: {refinement:?}"
        );
    }

    // --- Capability B, Phase 2c (TWO-PLAYER assumption discovery) -----------------------------------
    // The same 1-bit games as tests/exact_two_player_strategy.rs: `st' = c` (controller wins),
    // `st' = c & ¬e` (environment blocks with e=1), `st' = 0` (target unreachable).
    const GAME_CTRL: &str = "1 sort bitvec 1\n2 input 1 c\n3 input 1 e\n4 state 1 st\n5 zero 1\n6 init 1 4 5\n7 next 1 4 2\n";
    const GAME_ENVBLK: &str = "1 sort bitvec 1\n2 input 1 c\n3 input 1 e\n4 state 1 st\n5 zero 1\n6 init 1 4 5\n7 not 1 3\n8 and 1 2 7\n9 next 1 4 8\n";
    const GAME_STUCK: &str = "1 sort bitvec 1\n2 input 1 c\n3 input 1 e\n4 state 1 st\n5 zero 1\n6 init 1 4 5\n7 next 1 4 5\n";

    /// The wedge: `ENVBLK` (`st'=c&¬e`) is UNREALIZABLE (the environment blocks `st==1` with `e=1`), but
    /// under the environment assumption `e == 0` the controller wins (`st'=c`, set `c=1`). Discovery
    /// finds the non-vacuous `HoldsUnder(e == 0)` — the assume-guarantee assumption on the adversary.
    #[test]
    fn discover_game_env_assumption_finds_the_enabling_assumption() {
        let found = discover_game_env_assumption(GAME_ENVBLK, "st == 1", &["c"]);
        assert!(
            found
                .iter()
                .any(|a| a.phi == "e == 0" && a.kind == AssumptionKind::InputHold && a.non_vacuous),
            "expected a non-vacuous HoldsUnder(e == 0) making the game realizable: {found:?}"
        );
    }

    /// False-positive control: a game the controller ALREADY wins (`st'=c`, realizable) needs no
    /// assumption — discovery must return ∅, never fabricate an enabling φ.
    #[test]
    fn discover_game_env_assumption_empty_when_already_realizable() {
        let found = discover_game_env_assumption(GAME_CTRL, "st == 1", &["c"]);
        assert!(
            found.is_empty(),
            "a realizable game needs no environment assumption: {found:?}"
        );
    }

    /// Soundness control: an unrealizable game whose target is structurally UNREACHABLE (`st'=0`, so
    /// `st==1` is never reached under any env constraint) must NOT yield a spurious assumption — the
    /// realizability + non-vacuity gates both reject every env hold.
    #[test]
    fn discover_game_env_assumption_no_spurious_phi_on_dead_target() {
        let found = discover_game_env_assumption(GAME_STUCK, "st == 1", &["c"]);
        assert!(
            found.is_empty(),
            "a structurally-unreachable target must not fabricate an enabling φ: {found:?}"
        );
    }

    // TWO independent blockers: `st' = c & ¬e1 & ¬e2` (the environment blocks `st==1` by asserting e1 OR
    // e2). NO single hold wins (`e1==0` still blockable by e2, and vice-versa), so the CONJUNCTIVE slice
    // must discover the minimal `e1 == 0 && e2 == 0` — the edn/otbn "multiple blockers" shape in miniature.
    const GAME_TWOBLOCK: &str = "1 sort bitvec 1\n2 input 1 c\n3 input 1 e1\n4 input 1 e2\n5 state 1 st\n6 zero 1\n7 init 1 5 6\n8 not 1 3\n9 not 1 4\n10 and 1 2 8\n11 and 1 10 9\n12 next 1 5 11\n";

    /// The conjunctive slice: `st'=c&¬e1&¬e2` is UNREALIZABLE and NO single env-input hold makes it
    /// realizable, so `discover_game_env_assumption` falls back to the conjunction and finds the minimal
    /// `HoldsUnder(e1 == 0 && e2 == 0)` (`kind = InputConjunction`, non-vacuous).
    #[test]
    fn discover_game_env_conjunction_finds_the_minimal_conjunction() {
        let found = discover_game_env_assumption(GAME_TWOBLOCK, "st == 1", &["c"]);
        let conj = found
            .iter()
            .find(|a| a.kind == AssumptionKind::InputConjunction)
            .unwrap_or_else(|| panic!("expected a conjunctive assumption: {found:?}"));
        // Both blockers must be constrained (order-independent), and nothing else.
        assert!(conj.non_vacuous);
        assert!(
            conj.phi.contains("e1 == 0") && conj.phi.contains("e2 == 0") && conj.phi.contains("&&"),
            "expected the minimal 2-input conjunction e1==0 && e2==0: {}",
            conj.phi
        );
        assert!(
            !conj.phi.contains("c =="),
            "the controllable input c must not appear in the env assumption: {}",
            conj.phi
        );
    }

    // Like the real lift: the recovery target `busy` is a combinational OUTPUT (`busy = st != IDLE`),
    // NOT a state cell. IDLE(0)--go-->WORK(1); WORK--en=0-->FAULT(2,absorbing)/--en=1-->IDLE. `busy==0`
    // ⇔ `st==0`. Exercises the output-port resolution in the non-vacuity gate (`emit_ag_state_atom_monitor`
    // output fallback) — without it the gate errors on every `busy`/`done`/`valid`/`ready` output target.
    const EN_TRAP_OUTPUT: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 input 2 en
4 input 2 go
5 state 1 st
6 zero 1
7 init 1 5 6
8 one 1
9 constd 1 2
10 eq 2 5 6
11 eq 2 5 8
13 ite 1 4 8 6
14 ite 1 3 6 9
15 ite 1 11 14 9
16 ite 1 10 13 15
17 next 1 5 16
18 neq 2 5 6
19 output 18 busy
";

    /// Capability B slice 1 — the recovery target is a combinational OUTPUT (`busy`), the common real-RTL
    /// shape. Assumption discovery must find the non-vacuous `en == 1` hold, which requires the
    /// non-vacuity gate to resolve `busy` via the OUTPUT-port fallback (not just state cells). This is the
    /// exact gap that blocked the composition on the `compute_engine_faulty` lift (`busy` is an output).
    #[test]
    fn discover_assumptions_resolves_output_target() {
        let found = discover_assumptions(EN_TRAP_OUTPUT, "busy == 0", &[]);
        assert!(
            found.iter().any(|a| a.phi == "en == 1" && a.non_vacuous),
            "output-target non-vacuity gate must resolve `busy` and find HoldsUnder(en==1): {found:?}"
        );
    }

    // Phase 2b — a POSITIONAL-strategy trap: st cycles A(0)→B(1)→C(2)→A but the SAFE input DIFFERS per
    // state (x=0 at A, x=1 at B, x=0 at C); the wrong input at any state drops to TRAP(3, absorbing).
    // No CONSTANT hold recovers (x=0 traps at B, x=1 traps at A) — only a positional env strategy does.
    const POSITIONAL_TRAP: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 input 2 x
4 state 1 st
5 zero 1
6 init 1 4 5
7 one 1
8 constd 1 2
9 constd 1 3
10 eq 2 4 5
11 eq 2 4 7
12 eq 2 4 8
13 ite 1 3 9 7
14 ite 1 3 8 9
15 ite 1 3 9 5
16 ite 1 12 15 9
17 ite 1 11 14 16
18 ite 1 10 13 17
19 next 1 4 18
";

    /// Phase 2b end-to-end — `--discover-assumptions` on a positional-trap design where slice-1's
    /// constant holds find NOTHING falls back to the symbolic env-strategy synthesis and reports an
    /// `EnvStrategy` assumption. Canonical verdict stays VIOLATED (conditional-only).
    #[test]
    fn refined_verdict_falls_back_to_env_strategy_when_no_constant_hold() {
        let (verdict, refinement) =
            verify_recoverability_refined(POSITIONAL_TRAP, "st == 0", &[], &[], true);
        assert_eq!(
            verdict,
            PropertyVerdict::Violated,
            "free-input the positional trap is VIOLATED (unchanged)"
        );
        assert!(
            refinement
                .holds_under
                .iter()
                .any(|a| a.kind == AssumptionKind::EnvStrategy && a.non_vacuous),
            "no constant hold works ⇒ the symbolic env-strategy is reported: {:?}",
            refinement.holds_under
        );
    }

    /// Capability B slice 1 SOUNDNESS CONTROL — a structurally-dead target (`st` stuck at 1, `st==0`
    /// unreachable regardless of the input `x`) must yield NO discovered assumption: no input hold can
    /// make an unreachable `good` non-vacuously hold, so assumption discovery must NOT fabricate a φ.
    #[test]
    fn discover_assumptions_none_for_structurally_dead_target() {
        const DEAD: &str = "\
1 sort bitvec 1
2 input 1 x
3 state 1 st
4 one 1
5 init 1 3 4
6 next 1 3 4
";
        assert!(
            discover_assumptions(DEAD, "st == 0", &[]).is_empty(),
            "no input hold makes an unreachable target non-vacuously hold ⇒ no spurious assumption"
        );
    }

    /// Capability B slice 1 CONTROL — a design that already HOLDS free-input needs no assumption: with
    /// the flag set: the base HOLDS and the A→B composition runs on the operational cell (rst_n=1) but
    /// finds NO non-vacuous hold — RESET_DEP's operational trap (once `busy` is set via `start` it sticks)
    /// is unavoidable by a constant input hold (holding `start=0` is vacuous — `busy` never leaves 0). So
    /// `holds_under` stays empty (no noise / no false positive from the composition).
    #[test]
    fn refined_verdict_no_assumption_when_already_holds() {
        let (verdict, refinement) =
            verify_recoverability_refined(RESET_DEP, "busy == 0", &[], &[], true);
        assert_eq!(verdict, PropertyVerdict::Holds);
        assert!(
            refinement.holds_under.is_empty(),
            "the operational trap is unavoidable by a constant hold ⇒ no enabling assumption: {refinement:?}"
        );
    }

    // Free reset `rst` (active-low mux `ite(rst, op, 0)`) escapes any trap ⇒ base `AG EF(st==0)` HOLDS.
    // Operationally (rst=1) it is an INPUT-GATED trap: IDLE(0)--go-->WORK(1); WORK--en=0-->FAULT(2,
    // absorbing) / --en=1-->IDLE. `config_partition` localizes violated:[rst=1]; the A→B composition then
    // discovers the OPERATIONAL enabling hold `en==1` within that config.
    const RESET_GATE_TRAP: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 input 2 rst
4 input 2 go
5 input 2 en
6 state 1 st
7 zero 1
8 init 1 6 7
9 one 1
10 constd 1 2
11 eq 2 6 7
12 eq 2 6 9
13 ite 1 4 9 7
14 ite 1 5 7 10
15 ite 1 12 14 10
16 ite 1 11 13 15
17 ite 1 3 16 7
18 next 1 6 17
";

    // Like the LIFTED async-reset RTL (e.g. compute_engine_faulty): the reset mux establishes the start
    // state and there is NO `init` line. Active-low reset `rst_n`; operationally (rst_n=1) IDLE--start-->
    // WORK; WORK--err=1-->FAULT(absorbing) / --err=0-->IDLE. Without inject_reset_init, pinning rst_n=1
    // leaves a havoc init (could start in FAULT) ⇒ no hold recovers; WITH it the operational search starts
    // from the reset state (IDLE) ⇒ `rst_n==1 && err==0` recovers. This is the real-RTL composition shape.
    const CEF_LIKE_NO_INIT: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 input 2 rst_n
4 input 2 start
5 input 2 err
6 state 1 st
7 zero 1
9 one 1
10 constd 1 2
11 eq 2 6 7
12 eq 2 6 9
13 ite 1 4 9 7
14 ite 1 5 10 7
15 ite 1 12 14 10
16 ite 1 11 13 15
17 ite 1 3 16 7
18 next 1 6 17
";

    /// A→B COMPOSITION on the REAL-RTL shape (async reset, NO init line) — proves `inject_reset_init` in
    /// `operational_reset_init_model` is what makes the composition reach lifted RTL: the operational model
    /// starts from the reset state (IDLE), so discovery finds `rst_n == 1 && err == 0` (avoid the fault
    /// strobe). Without the init injection, the havoc start would leave `holds_under` empty.
    #[test]
    fn refined_verdict_composition_reaches_no_init_async_reset_shape() {
        let (verdict, refinement) =
            verify_recoverability_refined(CEF_LIKE_NO_INIT, "st == 0", &[], &[], true);
        assert_eq!(verdict, PropertyVerdict::Holds, "free reset ⇒ base HOLDS");
        assert!(
            refinement
                .config_partition
                .as_ref()
                .is_some_and(|p| p.violated.contains(&vec![("rst_n".to_string(), 1)])),
            "config_partition localizes operational rst_n=1: {:?}",
            refinement.config_partition
        );
        assert!(
            refinement
                .holds_under
                .iter()
                .any(|a| a.phi == "rst_n == 1 && err == 0" && a.non_vacuous),
            "reset-init operational composition finds rst_n==1 && err==0: {:?}",
            refinement.holds_under
        );
    }

    /// A→B COMPOSITION — the base `AG EF(st==0)` HOLDS via the free-reset escape, so `config_partition`
    /// (capability A) localizes the operational trap (violated:[rst=1]) and assumption discovery
    /// (capability B) runs WITHIN that config to surface the operational enabling assumption
    /// `rst == 1 && en == 1`. Capability A says WHICH config is bad; capability B says how to recover
    /// under it. Canonical verdict stays HOLDS (conditional-only, unchanged).
    #[test]
    fn refined_verdict_composes_config_partition_with_assumption_discovery() {
        let (verdict, refinement) =
            verify_recoverability_refined(RESET_GATE_TRAP, "st == 0", &[], &[], true);
        assert_eq!(
            verdict,
            PropertyVerdict::Holds,
            "free reset ⇒ AG EF(st==0) HOLDS (base, unchanged)"
        );
        let part = refinement
            .config_partition
            .as_ref()
            .expect("auto reset config_partition localizes the operational trap");
        assert!(
            part.violated.contains(&vec![("rst".to_string(), 1)]),
            "operational (rst=1) traps: {part:?}"
        );
        assert!(
            refinement
                .holds_under
                .iter()
                .any(|a| a.phi == "rst == 1 && en == 1" && a.non_vacuous),
            "A→B composition surfaces the operational enabling assumption: {:?}",
            refinement.holds_under
        );
    }

    /// Actionable-⊥ diagnosis — a recovery target riding only NARROW control state localizes NO
    /// structural obstacle (the abstraction may be closable with more predicates), so the hint says so.
    #[test]
    fn recoverability_bot_diagnosis_empty_when_control_only() {
        const D: &str = "1 sort bitvec 1\n2 sort bitvec 2\n3 state 1 flag\n4 state 2 st\n\
5 zero 1\n6 one 1\n7 zero 2\n8 one 2\n9 init 1 3 5\n10 init 2 4 7\n11 slice 1 4 0 0\n\
12 next 1 3 11\n13 add 2 4 8\n14 next 2 4 13\n";
        let diag = diagnose_recoverability_bot(D, "flag == 1");
        assert!(
            diag.is_empty(),
            "narrow control-only cone: no wide influence, no uncertified counter"
        );
        assert!(diag.hint().contains("no structural obstacle"));
    }

    /// Actionable-⊥ diagnosis on a YOSYS-LIFT shape — the gating obstacles are UNNAMED internal
    /// registers (as in i2c's `scl_padoen_o` cone: the 16-bit SCL prescaler is unnamed). Pre-fix the
    /// diagnosis was symbol-gated and returned `is_empty()` ("coarse-but-closable") — a false negative.
    /// The fix must localize the unnamed down-counter + unnamed wide datapath by `nid`.
    #[test]
    fn recoverability_bot_diagnosis_localizes_unnamed_lift_obstacles() {
        // flag' = (cnt == 0); cnt is an UNNAMED 8-bit down-counter; data is an UNNAMED 16-bit datapath
        // register whose value the recovery rides (flag also samples data's low bit).
        const D: &str = "1 sort bitvec 1\n2 sort bitvec 8\n3 sort bitvec 16\n\
4 state 1 flag\n5 state 2\n6 state 3\n\
7 zero 1\n8 zero 2\n9 zero 3\n10 one 2\n11 one 3\n12 constd 2 200\n\
13 init 1 4 7\n14 init 2 5 12\n15 init 3 6 9\n\
16 eq 1 5 8\n17 slice 1 6 0 0\n18 and 1 16 17\n19 next 1 4 18\n\
20 sub 2 5 10\n21 ite 2 16 8 20\n22 next 2 5 21\n23 add 3 6 11\n24 next 3 6 23\n";
        let diag = diagnose_recoverability_bot(D, "flag == 1");
        assert!(
            !diag.is_empty(),
            "the unnamed prescaler + datapath must be localized, not skipped by the symbol gate"
        );
        assert_eq!(
            diag.uncertified_counters,
            vec![("nid5(8-bit)".to_string(), 8)],
            "the unnamed 8-bit down-counter is reported by nid"
        );
        assert_eq!(
            diag.wide_influences,
            vec![("nid6(16-bit)".to_string(), 16)],
            "the unnamed 16-bit datapath the recovery rides is reported by nid"
        );
        assert!(diag.hint().contains("ranking certificate"));
    }

    /// Actionable-⊥ diagnosis when the recovery target is a top-level `output` (the lifted-design
    /// shape — i2c's `scl_padoen_o`). `collect_symbols` does not index outputs, so pre-fix the cone
    /// seeded empty and the diagnosis reported "no obstacle". The seed must resolve the output and
    /// walk its driving logic to localize the unnamed gating counter.
    #[test]
    fn recoverability_bot_diagnosis_seeds_from_output_atom() {
        // out = output(flag); flag' = (cnt == 0); cnt is an UNNAMED 8-bit down-counter.
        const D: &str = "1 sort bitvec 1\n2 sort bitvec 8\n3 state 1 flag\n4 state 2\n\
5 zero 1\n6 zero 2\n7 one 2\n8 constd 2 100\n9 init 1 3 5\n10 init 2 4 8\n\
11 eq 1 4 6\n12 next 1 3 11\n13 sub 2 4 7\n14 ite 2 11 6 13\n15 next 2 4 14\n16 output 3 out\n";
        let diag = diagnose_recoverability_bot(D, "out == 1");
        assert!(
            !diag.is_empty(),
            "the output-named good must seed the cone and localize the unnamed counter"
        );
        assert_eq!(
            diag.uncertified_counters,
            vec![("nid4(8-bit)".to_string(), 8)],
            "seeded from `output out`, the walk reaches the unnamed 8-bit down-counter"
        );
    }

    // Ranking certificate — the well-founded-descent / RANKING class. A down-counter to 0: `cnt`
    // decrements (`cnt' = ite(cnt==0, 0, cnt - step)`), so `AG EF (cnt == 0)` HOLDS by a well-founded
    // descent, but no bounded predicate set captures the 2^W-step descent → the cube abstains. The
    // ranking certificate proves it over the exact transition (δ = cnt strictly decreases, bounded below).
    fn downcnt_w(width: u32, init: u128, step: u64) -> String {
        format!(
            "1 sort bitvec 1\n2 sort bitvec {width}\n3 state 2 cnt\n4 zero 2\n5 constd 2 {step}\n\
             6 constd 2 {init}\n7 init 2 3 6\n8 eq 1 3 4\n9 sub 2 3 5\n10 ite 2 8 4 9\n11 next 2 3 10\n"
        )
    }

    #[test]
    fn ranking_certificate_decides_downcounter_at_scale() {
        // 48-bit down-counter to 0 (step 1): the cube abstains (ranking), the certificate decides Holds.
        assert_eq!(
            verify_recoverability_scalable(&downcnt_w(48, 140_737_488_355_328, 1), "cnt == 0", &[])
                .expect("decides"),
            PropertyVerdict::Holds,
            "48-bit down-counter recoverability decides Holds via the ranking certificate"
        );
        // OVERSHOOT (step 2 from an ODD start): cnt stays odd, wraps past 0, never == 0 → the property
        // does NOT hold. The certificate correctly FAILS (the BV difference jumps up at the wrap), so it
        // soundly abstains rather than fabricate Holds.
        assert_ne!(
            verify_recoverability_scalable(&downcnt_w(48, 140_737_488_355_327, 2), "cnt == 0", &[])
                .expect("decides"),
            PropertyVerdict::Holds,
            "the overshoot counter must NOT be a spurious Holds"
        );
        // DIFFERENTIAL ORACLE: the 8-bit down-counter routes through the exact BDD engine and agrees.
        assert_eq!(
            verify_recoverability(&downcnt_w(8, 200, 1), "cnt == 0").expect("exact decides"),
            PropertyVerdict::Holds,
            "8-bit exact oracle agrees with the 48-bit ranking Holds"
        );
    }

    // ∃-input ranking — a SOME-path descent. `cnt` decrements only when a free input `dec` is 1
    // (`cnt' = ite(cnt==0, 0, ite(dec, cnt-1, cnt))`), so `AG EF (cnt == 0)` HOLDS (the `dec=1` path
    // drains) but NOT `AG AF` (`dec=0` forever holds). The all-path certificate (A) fails; the ∃-input
    // certificate (B) decides it — the shape of a FIFO drained by only some input (read, not write).
    fn nondet_drain_w(width: u32, init: u128) -> String {
        format!(
            "1 sort bitvec 1\n2 sort bitvec {width}\n3 state 2 cnt\n4 zero 2\n5 one 2\n\
             6 constd 2 {init}\n7 init 2 3 6\n8 input 1 dec\n9 eq 1 3 4\n10 sub 2 3 5\n11 ite 2 8 10 3\n\
             12 ite 2 9 4 11\n13 next 2 3 12\n"
        )
    }

    #[test]
    fn ranking_certificate_exists_input_decides_nondeterministic_drain() {
        // 48-bit nondeterministic drain: AG EF (cnt==0) decides Holds via the ∃-input certificate (some
        // input path drains) where the all-path certificate fails (dec=0 holds cnt) and the cube abstains.
        assert_eq!(
            verify_recoverability_scalable(
                &nondet_drain_w(48, 140_737_488_355_328),
                "cnt == 0",
                &[]
            )
            .expect("decides"),
            PropertyVerdict::Holds,
            "48-bit nondeterministic drain decides Holds via the ∃-input ranking certificate"
        );
        // DIFFERENTIAL ORACLE at 8-bit (exact BDD).
        assert_eq!(
            verify_recoverability(&nondet_drain_w(8, 200), "cnt == 0").expect("exact decides"),
            PropertyVerdict::Holds,
            "8-bit exact oracle agrees with the 48-bit ∃-input ranking Holds"
        );
    }

    // Lexicographic ranking — a NESTED counter. `lo` cycles `lo_max → 0`; `hi` decrements ONLY when
    // `lo == 0`. `AG EF (hi == 0)` HOLDS, but δ = `hi` is NOT a valid ranking (hi holds while lo > 0, so
    // it does not strictly decrease every step). The lexicographic measure `(hi, lo)` decreases every
    // step (either `lo` down with `hi` fixed, or `hi` down when `lo` wraps) and is well-founded → decides.
    fn nested_w(width: u32, lo_max: u128, hi_init: u128) -> String {
        format!(
            "1 sort bitvec 1\n2 sort bitvec {width}\n3 state 2 hi\n4 state 2 lo\n5 zero 2\n6 one 2\n\
             7 constd 2 {lo_max}\n8 constd 2 {hi_init}\n9 init 2 3 8\n10 init 2 4 7\n11 eq 1 4 5\n\
             12 sub 2 4 6\n13 ite 2 11 7 12\n14 next 2 4 13\n15 eq 1 3 5\n16 sub 2 3 6\n\
             17 ite 2 15 5 16\n18 ite 2 11 17 3\n19 next 2 3 18\n"
        )
    }

    #[test]
    fn ranking_certificate_decides_nested_counter_via_lexicographic() {
        // 48-bit nested counter: `hi` alone is not a ranking; the lexicographic `(hi, lo)` decides Holds
        // where the single-register certificate fails, the cube abstains, and exact BDD walls.
        assert_eq!(
            verify_recoverability_scalable(&nested_w(48, 100, 140_737_488_355_328), "hi == 0", &[])
                .expect("decides"),
            PropertyVerdict::Holds,
            "48-bit nested counter decides Holds via the lexicographic ranking certificate"
        );
        // DIFFERENTIAL ORACLE at 8-bit (exact BDD).
        assert_eq!(
            verify_recoverability(&nested_w(8, 5, 200), "hi == 0").expect("exact decides"),
            PropertyVerdict::Holds,
            "8-bit exact oracle agrees with the 48-bit lexicographic ranking Holds"
        );
    }

    // Multi-component (k-tuple) lexicographic — a 3-LEVEL nested counter. `lo` cycles; `mid` decrements
    // iff `lo==0`; `hi` decrements iff `lo==0 && mid==0`. `AG EF (hi==0)` HOLDS, but NO single register
    // and NO 2-tuple `(hi, s)` is a ranking (when lo counts down, `(hi, mid)` holds; at the mid step
    // `(hi, lo)` sees lo wrap up). Only the FULL 3-tuple `(hi, mid, lo)` decreases every step.
    fn nested3_w(width: u32, lo_max: u128, mid_max: u128, hi_init: u128) -> String {
        format!(
            "1 sort bitvec 1\n2 sort bitvec {width}\n3 state 2 hi\n4 state 2 mid\n5 state 2 lo\n\
             6 zero 2\n7 one 2\n8 constd 2 {lo_max}\n9 constd 2 {mid_max}\n10 constd 2 {hi_init}\n\
             11 init 2 3 10\n12 init 2 4 9\n13 init 2 5 8\n14 eq 1 5 6\n15 sub 2 5 7\n16 ite 2 14 8 15\n\
             17 next 2 5 16\n18 eq 1 4 6\n19 sub 2 4 7\n20 ite 2 18 9 19\n21 ite 2 14 20 4\n\
             22 next 2 4 21\n23 and 1 14 18\n24 eq 1 3 6\n25 sub 2 3 7\n26 ite 2 24 6 25\n\
             27 ite 2 23 26 3\n28 next 2 3 27\n"
        )
    }

    #[test]
    fn ranking_certificate_decides_three_level_nested_via_full_tuple() {
        // 48-bit 3-level nested: only the full (hi, mid, lo) tuple decides — the single-register and
        // 2-tuple measures fail, the cube abstains, exact BDD walls.
        assert_eq!(
            verify_recoverability_scalable(
                &nested3_w(48, 50, 30, 140_737_488_355_328),
                "hi == 0",
                &[]
            )
            .expect("decides"),
            PropertyVerdict::Holds,
            "48-bit 3-level nested counter decides Holds via the full-tuple lexicographic ranking"
        );
        // DIFFERENTIAL ORACLE at 8-bit (exact BDD).
        assert_eq!(
            verify_recoverability(&nested3_w(8, 3, 2, 100), "hi == 0").expect("exact decides"),
            PropertyVerdict::Holds,
            "8-bit exact oracle agrees with the 48-bit full-tuple ranking Holds"
        );
    }

    // COI-filtered ∃-input enumeration — a drain gated on `dec` PLUS a DEAD wide input `junk` (declared,
    // never used). The enumeration pins only inputs in the good register's next-cone (`dec`); the dead
    // 48-bit input is left free (sound), so it does not swamp the enumeration's bit cap. This is the
    // shape a real hardened primitive (OpenTitan `prim_count`, with its secondary counter + FPV backdoor)
    // presents — inputs that do not touch the ranking register but would otherwise block the enumeration.
    fn dead_input_drain_w(width: u32, init: u128) -> String {
        format!(
            "1 sort bitvec 1\n2 sort bitvec {width}\n3 state 2 cnt\n4 zero 2\n5 one 2\n\
             6 constd 2 {init}\n7 init 2 3 6\n8 input 1 dec\n9 input 2 junk\n10 eq 1 3 4\n\
             11 sub 2 3 5\n12 ite 2 8 11 3\n13 ite 2 10 4 12\n14 next 2 3 13\n"
        )
    }

    #[test]
    fn ranking_certificate_ignores_dead_wide_input() {
        // The dead 48-bit input would push the total enumerated input bits over the cap; the good-cone
        // filter excludes it, so the ∃-input drain (`dec`) still decides Holds.
        assert_eq!(
            verify_recoverability_scalable(
                &dead_input_drain_w(48, 140_737_488_355_328),
                "cnt == 0",
                &[]
            )
            .expect("decides"),
            PropertyVerdict::Holds,
            "the dead wide input is excluded from the ∃-input enumeration → decides Holds"
        );
        // DIFFERENTIAL ORACLE at 8-bit (exact BDD).
        assert_eq!(
            verify_recoverability(&dead_input_drain_w(8, 200), "cnt == 0").expect("exact decides"),
            PropertyVerdict::Holds,
            "8-bit exact oracle agrees"
        );
    }

    // Ascent ranking — a saturating UP-counter to MAX = 2^W-1. `AG EF (cnt == MAX)` ("always able to fill
    // up") HOLDS by an ASCENT, but the descent measure δ = cnt - MAX wraps; the ASCENT measure δ = MAX -
    // cnt strictly decreases every step. The certificate tries both directions.
    fn upcnt_w(width: u32) -> String {
        format!(
            "1 sort bitvec 1\n2 sort bitvec {width}\n3 state 2 cnt\n4 ones 2\n5 one 2\n6 zero 2\n\
             7 init 2 3 6\n8 eq 1 3 4\n9 add 2 3 5\n10 ite 2 8 4 9\n11 next 2 3 10\n"
        )
    }

    #[test]
    fn ranking_certificate_decides_ascent_to_max() {
        // 48-bit up-counter to MAX: decides Holds via the ascent (δ = V - r) direction, where the descent
        // direction wraps, the cube abstains, and exact BDD walls.
        let max48: u128 = (1u128 << 48) - 1;
        assert_eq!(
            verify_recoverability_scalable(&upcnt_w(48), &format!("cnt == {max48}"), &[])
                .expect("decides"),
            PropertyVerdict::Holds,
            "48-bit up-counter to MAX decides Holds via the ascent ranking"
        );
        // DIFFERENTIAL ORACLE at 8-bit (exact BDD).
        assert_eq!(
            verify_recoverability(&upcnt_w(8), "cnt == 255").expect("exact decides"),
            PropertyVerdict::Holds,
            "8-bit exact oracle agrees with the 48-bit ascent ranking Holds"
        );
    }

    // === Auto-escalation: the public verb transparently uses the cube path ======

    #[test]
    fn verify_recoverability_auto_escalates_on_wide_design() {
        // The public `verify_recoverability` tries exact first; on the over-cap wide
        // designs it escalates to the cube path and returns the definite verdict.
        assert_eq!(
            verify_recoverability(WIDE_RECOVERABLE, "st == 0").expect("decides"),
            PropertyVerdict::Holds,
            "auto-escalation must yield the cube-decided Holds on the wide design"
        );
        assert_eq!(
            verify_recoverability(WIDE_TRAP, "st == 0").expect("decides"),
            PropertyVerdict::Violated,
            "auto-escalation must yield the cube-decided Violated on the wide trap"
        );
    }

    #[test]
    fn verify_recoverability_keeps_exact_verdict_when_exact_decides() {
        // When the exact engine decides (small fixtures), the escalation is a no-op —
        // the exact verdict is returned unchanged.
        assert_eq!(
            verify_recoverability(RESPONDER, "st == 0").expect("decides"),
            PropertyVerdict::Holds
        );
        assert_eq!(
            verify_recoverability(STALLER, "st == 0").expect("decides"),
            PropertyVerdict::Violated
        );
    }

    // === Abstains + surface plumbing ===========================================

    #[test]
    fn scalable_non_equality_target_abstains() {
        // A non-`==` good atom is an honest Unknown (the cube auto-seed only pins `==`
        // targets), never a fabricated definite verdict.
        assert_eq!(
            verify_recoverability_scalable(RESPONDER, "st < 3", &[]).expect("abstains cleanly"),
            PropertyVerdict::Unknown
        );
    }

    #[test]
    fn scalable_honors_extra_predicates() {
        // Extra abstraction predicates refine the cube without changing the (sound)
        // verdict on a design the seed already decides.
        let extra = vec![parse_extra_predicate("busy:st=1").expect("parses")];
        assert_eq!(
            verify_recoverability_scalable(RESPONDER, "st == 0", &extra).expect("decides"),
            PropertyVerdict::Holds
        );
    }

    #[test]
    fn parse_extra_predicate_parses_and_rejects() {
        assert_eq!(
            parse_extra_predicate("idle:state_q=3").expect("parses"),
            PredicateSpec {
                name: "idle".into(),
                register: "state_q".into(),
                value: 3,
            }
        );
        assert!(parse_extra_predicate("no_colon").is_err());
        assert!(parse_extra_predicate("n:reg").is_err());
        assert!(parse_extra_predicate("n:reg=notanumber").is_err());
    }

    // === b2: counter-abstraction recoverability =================================

    /// A `w`-bit down-counter `cnt` (reload 4 at expiry, decrement otherwise) gating a
    /// 3-state FSM `idle→A→{B|idle}→idle` that advances ONLY when `cnt==0`. When
    /// `reaches_b` the FSM cycles through `B` (so `AG EF (fsm==2)` HOLDS); otherwise `B`
    /// is unreachable (so it is VIOLATED). This is the FSM-gated-by-counter shape b2
    /// targets — the counter gates progress but is not itself the recoverability target.
    fn fsm_gated_counter(w: usize, reaches_b: bool) -> String {
        let reload = format!("{:0width$b}", 4, width = w);
        let dec = format!("{:0width$b}", 1, width = w);
        let advance_from_a = if reaches_b { 16 } else { 14 }; // A → B, or A → idle
        format!(
            r#"
1 sort bitvec {w}
2 sort bitvec 2
3 sort bitvec 1
4 state 1 cnt
5 state 2 fsm
6 const 1 {reload}
7 const 1 {dec}
8 zero 1
9 eq 3 4 8
10 sub 1 4 7
11 ite 1 9 6 10
12 next 1 4 11
13 init 1 4 6
14 zero 2
15 const 2 01
16 const 2 10
17 eq 3 5 14
18 eq 3 5 15
19 ite 2 18 {advance_from_a} 14
20 ite 2 17 15 19
21 ite 2 9 20 5
22 next 2 5 21
23 init 2 5 14
"#
        )
    }

    /// b2 SOUNDNESS — on the SMALL (exact-decidable) recoverable model the exact engine
    /// is the ground-truth oracle (Holds), and b2 must AGREE. This is the differential
    /// oracle for the counter-abstraction composition.
    #[test]
    fn b2_matches_exact_oracle_on_recoverable_fsm() {
        use crate::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};
        let small = fsm_gated_counter(3, true);
        let f = mu_parser::parse("nu Y. ((mu X. ((fsm == 2) || <> X)) && [] Y)").unwrap();
        // ORACLE: the exact engine decides the small model directly.
        assert_eq!(
            exact_symbolic_verdict(&small, &f).expect("exact decides small"),
            ExactVerdict::Holds,
            "ground truth: the FSM cycles through B forever"
        );
        // b2 on the same model → the same definite verdict.
        let file = crate::adapter::btor2::parser::parse(&small).unwrap();
        assert_eq!(
            verify_recoverability_counter_abstracted(&file, "fsm == 2", &["fsm".to_string()]),
            Some(PropertyVerdict::Holds),
            "b2 must agree with the exact oracle"
        );
    }

    /// b2 SOUNDNESS — b2 must NEVER fabricate a `Holds`. On the unrecoverable model the
    /// oracle says Violated; b2 must abstain (`None`), since only a `Holds` on the
    /// over-approximating abstract model is sound.
    #[test]
    fn b2_abstains_on_unrecoverable_fsm_never_fabricates_holds() {
        use crate::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};
        let trap = fsm_gated_counter(3, false);
        let f = mu_parser::parse("nu Y. ((mu X. ((fsm == 2) || <> X)) && [] Y)").unwrap();
        // ORACLE: B is unreachable ⇒ AG EF (fsm==2) is Violated.
        assert_eq!(
            exact_symbolic_verdict(&trap, &f).expect("exact decides trap"),
            ExactVerdict::Violated,
            "ground truth: B is unreachable"
        );
        // b2 must NOT return Holds — abstain (the cube then decides Violated).
        let file = crate::adapter::btor2::parser::parse(&trap).unwrap();
        assert_eq!(
            verify_recoverability_counter_abstracted(&file, "fsm == 2", &["fsm".to_string()]),
            None,
            "b2 must abstain (never fabricate Holds) on a real Violated"
        );
    }

    /// b2 SIZE LEVER — a 40-bit counter puts the exact cone over the bit-blast cap, so
    /// the exact engine cannot decide it directly; the public escalating entry routes
    /// through b2, which collapses the counter and decides Holds.
    #[test]
    fn b2_decides_wide_counter_gated_recoverability() {
        use crate::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};
        let wide = fsm_gated_counter(40, true);
        let f = mu_parser::parse("nu Y. ((mu X. ((fsm == 2) || <> X)) && [] Y)").unwrap();
        // Until A2 of the engine-performance roadmap this guarded that the 40-bit counter's
        // 2^40-step diameter exceeded the exact engine (its iteration budget; the bit cap is
        // 192), so the escalation to b2 was what decided. The squaring rescue now closes that
        // chain in ~40 relational products, so the exact engine decides the wide design itself —
        // and becomes the oracle b2's answer is checked against.
        assert_eq!(
            exact_symbolic_verdict(&wide, &f).expect("the exact engine decides via squaring"),
            ExactVerdict::Holds,
            "exact oracle on the 40-bit gated counter"
        );
        // The escalating public entry decides Holds — now by the exact engine first.
        assert_eq!(
            verify_recoverability(&wide, "fsm == 2").expect("the wide design decides"),
            PropertyVerdict::Holds,
        );
        // b2 on its own still agrees with the oracle (the escalation it used to provide).
        let file = crate::adapter::btor2::parser::parse(&wide).unwrap();
        assert_eq!(
            verify_recoverability_counter_abstracted(&file, "fsm == 2", &["fsm".to_string()]),
            Some(PropertyVerdict::Holds),
            "b2 must agree with the exact oracle"
        );
    }

    /// P1-a shot-①b — the array-content Select seed on a yosys-lifted keep-`$mem` design whose
    /// recovery is gated on array content (`mem[key] == all-ones`). The read index is a reset-mux
    /// `ite(rst, key, 0)` over an UNNAMED FF, with the symbol on a width-preserving observable copy
    /// `uext(idx, 0) = u.key` — so `select_guard_atoms` must SEE THROUGH the uext to resolve the
    /// index. Isolates discovery (does the Select seed get produced?) from decision (can the cube
    /// use it?). The exact ROBDD SKIPs (in-cone array), so this is the escalated cube path.
    const AGR_SMALL_MEM: &str = "\
1 sort bitvec 1
2 input 1 clk
3 input 1 rst_n
4 input 1 start
5 sort bitvec 2
6 input 5 waddr
7 input 5 wdata
8 const 1 0
9 state 1
10 ite 1 3 9 8
11 output 10 busy
12 uext 1 10 0 u.busy
13 uext 1 2 0 u.clk
14 const 5 00
15 state 5
16 ite 5 3 15 14
17 uext 5 16 0 u.key
18 uext 1 3 0 u.rst_n
19 uext 1 4 0 u.start
20 uext 5 6 0 u.waddr
21 uext 5 7 0 u.wdata
22 sort array 5 5
23 state 22 u.mem
24 read 5 23 16
25 const 5 11
26 eq 1 24 25
27 ite 1 26 8 10
28 ite 1 10 27 10
29 const 1 1
30 not 1 10
31 and 1 4 30
32 ite 1 31 29 28
33 ite 1 3 32 8
34 next 1 9 33
35 ite 5 31 6 16
36 ite 5 3 35 14
37 next 5 15 36
38 read 5 23 6
39 not 5 25
40 and 5 38 39
41 and 5 7 25
42 or 5 41 40
43 write 22 23 6 42
44 redor 1 25
45 ite 22 44 43 23
46 next 22 23 45 u.mem
";

    #[test]
    fn p1a_select_guard_discovery_sees_through_resetmux_uext() {
        // (a) DISCOVERY: the index (`16 ite …`, unsymboled) resolves to `u.key` via the
        //     `17 uext 5 16 0 u.key` observable-copy see-through; array is the `$mem` state.
        let file = crate::adapter::btor2::parser::parse(AGR_SMALL_MEM).expect("parse");
        let atoms = select_guard_atoms(&file);
        assert!(
            atoms.contains(&("u.mem".to_string(), "u.key".to_string(), 3)),
            "select_guard_atoms must discover mem[key]==3 (via reset-mux/uext see-through); got {atoms:?}"
        );
    }

    /// Hand-authored twin of `AGR_SMALL_MEM` with NAMED state FFs and a DIRECT `mem[key]`
    /// read (no yosys reset-mux / observable-copy artifacts), so the reset-cube gate pins
    /// every register and the ONLY hard axis is the in-cone array content. Recovery: busy→0
    /// iff mem[key]==all-ones(3); mem is freely env-writable (mem[waddr]=wdata), so from any
    /// busy state the env can write mem[key]=3 and recover ⇒ AG EF(busy==0) HOLDS.
    const AGR_CLEAN: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 sort array 2 2
4 input 1 start
5 input 2 waddr
6 input 2 wdata
7 state 1 busy
8 state 2 key
9 state 3 mem
10 const 1 0
11 init 1 7 10
12 const 2 00
13 init 2 8 12
14 const 2 11
15 read 2 9 8
16 eq 1 15 14
17 not 1 7
18 and 1 4 17
19 const 1 1
20 and 1 7 16
21 ite 1 20 10 7
22 ite 1 18 19 21
23 next 1 7 22
24 ite 2 18 5 8
25 next 2 8 24
26 write 3 9 5 6
27 next 3 9 26
";

    #[test]
    fn p1a_spcr_decides_array_gated_recovery_holds() {
        // (b) THE WALL, REMOVED by SPCR (shot ①b). BACKGROUND: a definite recoverability verdict
        //     needs the diamond `<>X` = True, hence a MUST (hyper) edge; the must-edge ∀∃ —
        //     `∀ s ⊨ src. ∃ in, s'. (T ∧ s' ⊨ tgt)` — universally quantifies EVERY next-state
        //     component including the next-state ARRAY (`universal_bound_arrays`), which is
        //     quantified array logic (AUFBV + ∀-over-array), UNDECIDABLE for Z3 → no must-edges →
        //     the raw cube abstains. FIX: SPCR (`array_prophecy`) registerizes the property-
        //     ACCESSED cell (`pv = mem[key]`, exact frame) and DROPS the array, so the must-query
        //     is pure QF_BV (decidable). The design is then array-free and the exact ROBDD decides.
        //     Verdict-preserving reformulation ⇒ the `Holds` transfers (Bruns–Godefroid), matching
        //     the whole-array-registerized ROBDD oracle — but at O(#accessed-cells), the scaling
        //     that beats whole-array registerization (see `array_gates_recovery_large`).
        use crate::adapter::btor2::symbolic_bitblast::exact_symbolic_verdict;
        // Discovery still fires on the array-bearing form (the SPCR read-set detector).
        let file = crate::adapter::btor2::parser::parse(AGR_CLEAN).expect("parse");
        assert!(
            select_guard_atoms(&file).contains(&("mem".to_string(), "key".to_string(), 3)),
            "clean fixture must discover mem[key]==3"
        );
        let f = mu_parser::parse("nu Y. ((mu X. ((busy == 0) || <> X)) && [] Y)").unwrap();
        // The ARRAY-BEARING design makes the exact ROBDD SKIP (in-cone array) — the raw wall.
        assert!(
            exact_symbolic_verdict(AGR_CLEAN, &f).is_err(),
            "the in-cone array must make the raw exact ROBDD SKIP"
        );
        // SPCR eliminates it → the escalating scalable entry now DECIDES Holds.
        assert_eq!(
            verify_recoverability_scalable(AGR_CLEAN, "busy == 0", &[]).expect("scalable decides"),
            PropertyVerdict::Holds,
            "SPCR removes the array-must wall: registerize the accessed cell, drop the array, \
             the QF_BV must-query decides AG EF(busy==0) = Holds"
        );
    }
}
