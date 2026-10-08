//! mununu#629 — propagate a same-cycle antecedent's constant through the consequent's cone.
//!
//! The address-mux half of mununu#602. A property about one field of a wide packed record is
//! usually phrased through the record's read mux:
//!
//! ```text
//! (addr == 20'h00004) |-> (rdata == status_word)      // rdata = addr == k ? word_k : …
//! ```
//!
//! and bit `i` of `rdata` depends on bit `i` of EVERY arm, so no cone computed from the netlist
//! alone — not even the bit-level one — sees that `addr == 4` selects one arm (measured on the
//! consumer's shape: 2,213 bits, over the exact engine's cap). What the netlist cannot say the
//! formula does: for a **same-cycle** implication the antecedent fixes the signal's value wherever
//! the consequent is evaluated,
//!
//! ```text
//! AG( sig == K  ->  C(sig) )   ≡   AG( sig == K  ->  C[sig := K] )
//! ```
//!
//! because where the antecedent holds `sig` *is* `K` in that cycle, and where it does not the
//! implication is true regardless. So this pass, for every implication `Or(Not(sig == K), ψ)` of
//! positive polarity in the formula, mints for each register `r` read by a consequent atom whose
//! combinational cone reaches `sig` a copy `r__<sig>_eq_<K>` of that cone with `sig` replaced by
//! the constant and the result constant-folded — the `eq`/`ite` chain of a mux collapses to the
//! selected arm — and renames the atom to read the copy. The bit cone of the rewritten property is
//! then the one arm (96 bits on the consumer's shape), and the exact engine decides.
//!
//! **When `sig` is a primary input**, the antecedent cannot stay: the exact engine leaves inputs
//! free and quantified out by the modalities, so a state atom over an input is refused (it would
//! decouple the atom's copy of the input from the transition's). The universal reading the SVA
//! means — ∀ input: `sig == K → C(sig)` — is exactly `C[sig := K]` when every input value is
//! admissible, so the implication is replaced by its rewritten consequent. That step is taken only
//! when (a) no `constraint` / `fair` / `justice` line reads `sig` (a constraint could make `K`
//! inadmissible in some state, where the implication is vacuous but `C[sig := K]` is not — the
//! unsound direction for VIOLATED), (b) after the rewrite no atom of the formula reads `sig` at all
//! (so the ∀ distributes: nothing else depends on the input), and (c) the implication is of
//! positive polarity (under a `Not`, ∀ does not distribute into the implication). When `sig` is a
//! state cell the antecedent stays, as a state atom like any other.
//!
//! **Soundness.** Per implication the rewrite is an equivalence on the model (the substitution
//! lemma above), so no verdict can move; a rewrite the pass cannot make sound — a `Select` atom,
//! a constrained input, a register `K` cannot hold — is declined for that implication and the
//! formula is left as it was. `ExactSymbolicOptions::antecedent_propagate_enabled` /
//! `MUNUNU_NO_ANTECEDENT_PROPAGATE=1` disable it for a differential run.

use std::collections::{HashMap, HashSet};

use super::ast::{Btor2File, ConstValue, Line, Nid, Node, Op, Operand, Sort};
use super::parser;
use super::predicate_expr::{CmpOp, PredicateExpr, parse_predicate_atom_bool};
use crate::mu_calculus::{Formula, Node as MuNode, NodeId};

/// One consequent atom to rewrite: its node, its expression, the registers to mint copies for.
type PlannedAtom = (NodeId, PredicateExpr, Vec<(String, Nid)>);

/// The rewritten model and property, with one record per implication rewritten.
#[derive(Debug)]
pub struct Propagated {
    pub file: Btor2File,
    pub formula: Formula,
    /// `(signal, constant, antecedent dropped)` per rewritten implication.
    pub rewrites: Vec<(String, u64, bool)>,
}

/// Apply the pass. `None` when no implication qualifies (nothing changes).
pub fn propagate(file: &Btor2File, formula: &Formula) -> Option<Propagated> {
    let symbols = parser::collect_symbols(file);
    let mut op_or_output: HashMap<&str, Nid> = HashMap::new();
    for l in &file.lines {
        match &l.node {
            Node::Op {
                symbol: Some(s), ..
            } => {
                op_or_output.entry(s.as_str()).or_insert(l.nid);
            }
            Node::Output {
                symbol: Some(s),
                signal,
            } => {
                op_or_output.entry(s.as_str()).or_insert(signal.nid());
            }
            _ => {}
        }
    }
    // A name's binding node: the leaf it is a strict alias of, else the named Op / Output.
    let bind = |name: &str| -> Option<Nid> {
        if let Some(n) = parser::resolve_state_alias(file, name, true) {
            return Some(n);
        }
        if let Some((nid, _)) = symbols.iter().find(|(nid, s)| {
            s.as_str() == name
                && matches!(
                    file.lookup(**nid).map(|l| &l.node),
                    Some(Node::Input { .. })
                )
        }) {
            return Some(*nid);
        }
        op_or_output.get(name).copied()
    };
    let widths: HashMap<Nid, u32> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Sort {
                sort: Sort::BitVec { width },
            } => Some((l.nid, *width)),
            _ => None,
        })
        .collect();
    let sort_of = |nid: Nid| -> Option<Nid> {
        match &file.lookup(nid)?.node {
            Node::State { sort, .. } | Node::Input { sort, .. } | Node::Const { sort, .. } => {
                Some(*sort)
            }
            Node::Op { sort, .. } => Some(*sort),
            _ => None,
        }
    };
    // Side conditions read these leaves (a constrained input cannot have its antecedent dropped).
    let mut constrained: HashSet<Nid> = HashSet::new();
    for l in &file.lines {
        let sigs: Vec<Nid> = match &l.node {
            Node::Constraint { signal } | Node::Fair { signal } => vec![signal.nid()],
            Node::Justice { signals } => signals.iter().map(|s| s.nid()).collect(),
            _ => continue,
        };
        for s in sigs {
            constrained.extend(combinational_leaves(file, s));
        }
    }

    // The implications: `Or(Not(Predicate(sig == K)), ψ)` at positive polarity.
    let polarity = positive_polarity(formula);
    struct Impl {
        or_id: NodeId,
        cons_id: NodeId,
        sig: String,
        sig_nid: Nid,
        k: u64,
        atoms: Vec<NodeId>,
    }
    let mut impls: Vec<Impl> = Vec::new();
    for (i, node) in formula.nodes().iter().enumerate() {
        let or_id = NodeId(i);
        let MuNode::Or(l, r) = node else {
            continue;
        };
        if !polarity.contains(&or_id) {
            continue;
        }
        for (ante, cons) in [(*l, *r), (*r, *l)] {
            let MuNode::Not(inner) = formula.node(ante) else {
                continue;
            };
            let MuNode::Predicate(a) = formula.node(*inner) else {
                continue;
            };
            let Ok(PredicateExpr::Cmp {
                register,
                op: CmpOp::Eq,
                value,
            }) = parse_predicate_atom_bool(a)
            else {
                continue;
            };
            let Some(sig_nid) = bind(&register) else {
                continue;
            };
            if !matches!(
                file.lookup(sig_nid).map(|l| &l.node),
                Some(Node::State { .. } | Node::Input { .. })
            ) {
                continue; // a combinational antecedent: not a leaf to substitute
            }
            let Some(w) = sort_of(sig_nid).and_then(|s| widths.get(&s).copied()) else {
                continue;
            };
            if w < 64 && value >> w != 0 {
                continue; // `K` does not fit: the antecedent is never true, nothing to do
            }
            let mut atoms = Vec::new();
            collect_boolean_atoms(formula, cons, &mut atoms);
            impls.push(Impl {
                or_id,
                cons_id: cons,
                sig: register,
                sig_nid,
                k: value,
                atoms,
            });
            break;
        }
    }
    if impls.is_empty() {
        return None;
    }

    // The emitter for minted lines.
    let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut appended: Vec<Line> = Vec::new();
    let mut sort_of_width: HashMap<u32, Nid> = widths.iter().map(|(n, w)| (*w, *n)).collect();
    let mut consts: HashMap<(u32, u64), Nid> = HashMap::new();
    let mut new_nodes: Vec<MuNode> = formula.nodes().to_vec();
    let mut rewrites: Vec<(String, u64, bool)> = Vec::new();
    let mut dropped: Vec<(NodeId, NodeId)> = Vec::new();

    for im in &impls {
        // Which nodes depend on `sig`, combinationally.
        let dep = dependents(file, im.sig_nid);
        // Rewrite every consequent atom; decline the whole implication if any atom cannot be.
        let mut planned: Vec<PlannedAtom> = Vec::new();
        let mut ok = true;
        for atom_id in &im.atoms {
            let MuNode::Predicate(text) = formula.node(*atom_id) else {
                continue;
            };
            let Ok(expr) = parse_predicate_atom_bool(text) else {
                ok = false;
                break;
            };
            if matches!(expr, PredicateExpr::Select { .. }) || render(&expr).is_none() {
                ok = false;
                break;
            }
            let mut to_mint: Vec<(String, Nid)> = Vec::new();
            for r in expr.registers() {
                let Some(nid) = bind(&r) else {
                    ok = false;
                    break;
                };
                if dep.contains(&nid) {
                    to_mint.push((r, nid));
                }
            }
            if !ok {
                break;
            }
            planned.push((*atom_id, expr, to_mint));
        }
        if !ok {
            continue;
        }
        let is_input = matches!(
            file.lookup(im.sig_nid).map(|l| &l.node),
            Some(Node::Input { .. })
        );
        // Mint the copies and rename the atoms.
        let k_sort_w = sort_of(im.sig_nid)
            .and_then(|s| widths.get(&s).copied())
            .unwrap_or(1);
        let k_nid = const_nid(
            k_sort_w,
            im.k,
            &mut consts,
            &mut sort_of_width,
            &mut appended,
            &mut next_nid,
        );
        let mut memo: HashMap<Nid, Nid> = HashMap::new();
        for (atom_id, expr, to_mint) in &planned {
            let mut renames: HashMap<String, String> = HashMap::new();
            for (r, nid) in to_mint {
                let copy = copy_subst(
                    file,
                    *nid,
                    im.sig_nid,
                    k_nid,
                    &dep,
                    &mut consts,
                    &mut sort_of_width,
                    &mut appended,
                    &mut next_nid,
                    &mut memo,
                );
                let name = format!("{r}__{}_eq_{}", im.sig, im.k);
                // The copy is a node; give it the name by a width-preserving `uext 0` alias when
                // it is not already a fresh Op we can label (a reused constant, say).
                let labelled = label(
                    copy,
                    &name,
                    file,
                    &widths,
                    &mut sort_of_width,
                    &mut appended,
                    &mut next_nid,
                );
                renames.insert(r.clone(), labelled);
            }
            let renamed = rename_registers(expr, &renames);
            let text = render(&renamed).expect("renderable: checked above");
            new_nodes[atom_id.0] = MuNode::Predicate(text);
        }
        rewrites.push((im.sig.clone(), im.k, false));
        if is_input && !constrained.contains(&im.sig_nid) {
            dropped.push((im.or_id, im.cons_id));
        }
    }
    if rewrites.is_empty() {
        return None;
    }

    // Drop the antecedent of an input implication only when, after every rewrite and with every
    // candidate drop applied, no atom still reachable from the root reads that input — then the
    // ∀ over the input distributes and the implication IS its rewritten consequent. The check is
    // joint: two implications over one input drop together or not at all.
    let reachable = reachable_predicates(&new_nodes, formula.root(), &dropped);
    let reads_input = |text: &str, sig_nid: Nid| -> bool {
        let dep = dependents(file, sig_nid);
        parse_predicate_atom_bool(text)
            .ok()
            .map(|e| e.registers())
            .unwrap_or_default()
            .iter()
            .any(|r| bind(r).is_some_and(|n| dep.contains(&n)))
    };
    for (or_id, cons_id) in &dropped {
        let sig_nid = impls
            .iter()
            .find(|im| im.or_id == *or_id)
            .map(|im| im.sig_nid)
            .expect("a dropped implication was scanned");
        let still_read = reachable.iter().any(|p| match &new_nodes[p.0] {
            MuNode::Predicate(t) => reads_input(t, sig_nid),
            _ => false,
        });
        if still_read {
            continue;
        }
        // The `Or` becomes its consequent. The antecedent's nodes are now unreachable, but the
        // engine's atom scan walks EVERY node of the arena, so the orphaned `Predicate` is
        // neutralised to `True` rather than left to pin the input from the dead branch.
        let MuNode::Or(l, r) = formula.node(*or_id) else {
            unreachable!("scanned as an Or");
        };
        let ante_id = if *l == *cons_id { *r } else { *l };
        if let MuNode::Not(inner) = formula.node(ante_id) {
            new_nodes[inner.0] = MuNode::True;
            new_nodes[ante_id.0] = MuNode::False;
        }
        new_nodes[or_id.0] = new_nodes[cons_id.0].clone();
        if let Some(im) = impls.iter().find(|im| im.or_id == *or_id)
            && let Some(rw) = rewrites
                .iter_mut()
                .find(|(s, k, _)| *s == im.sig && *k == im.k)
        {
            rw.2 = true;
        }
    }

    let mut lines = file.lines.clone();
    lines.extend(appended);
    let by_nid = lines.iter().enumerate().map(|(i, l)| (l.nid, i)).collect();
    Some(Propagated {
        file: Btor2File { lines, by_nid },
        formula: Formula::new(formula.root(), new_nodes, formula.vars().to_vec()),
        rewrites,
    })
}

/// Node ids reachable from the root with an even number of `Not` ancestors.
fn positive_polarity(formula: &Formula) -> HashSet<NodeId> {
    fn walk(
        f: &Formula,
        id: NodeId,
        positive: bool,
        out: &mut HashSet<NodeId>,
        seen: &mut HashSet<(NodeId, bool)>,
    ) {
        if !seen.insert((id, positive)) {
            return;
        }
        if positive {
            out.insert(id);
        }
        match f.node(id) {
            MuNode::Not(x) => walk(f, *x, !positive, out, seen),
            MuNode::And(a, b) | MuNode::Or(a, b) => {
                walk(f, *a, positive, out, seen);
                walk(f, *b, positive, out, seen);
            }
            MuNode::Modal { target, .. } => walk(f, *target, positive, out, seen),
            MuNode::Mu { body, .. } | MuNode::Nu { body, .. } => {
                walk(f, *body, positive, out, seen)
            }
            _ => {}
        }
    }
    let mut out = HashSet::new();
    walk(formula, formula.root(), true, &mut out, &mut HashSet::new());
    out
}

/// The `Predicate` ids reachable from `root` in `nodes`, with each `(or, cons)` pair of
/// `replaced` read as its consequent (the antecedent side becomes unreachable).
fn reachable_predicates(
    nodes: &[MuNode],
    root: NodeId,
    replaced: &[(NodeId, NodeId)],
) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    while let Some(n) = stack.pop() {
        if !seen.insert(n) {
            continue;
        }
        let n = replaced
            .iter()
            .find(|(o, _)| *o == n)
            .map_or(n, |(_, c)| *c);
        match &nodes[n.0] {
            MuNode::Predicate(_) => out.push(n),
            MuNode::Not(x) => stack.push(*x),
            MuNode::And(a, b) | MuNode::Or(a, b) => {
                stack.push(*a);
                stack.push(*b);
            }
            MuNode::Modal { target, .. } => stack.push(*target),
            MuNode::Mu { body, .. } | MuNode::Nu { body, .. } => stack.push(*body),
            _ => {}
        }
    }
    out
}

/// `Predicate` ids reachable from `id` through `And` / `Or` / `Not` only — the atoms evaluated
/// in the same state as the antecedent.
fn collect_boolean_atoms(f: &Formula, id: NodeId, out: &mut Vec<NodeId>) {
    match f.node(id) {
        MuNode::Predicate(_) => out.push(id),
        MuNode::Not(x) => collect_boolean_atoms(f, *x, out),
        MuNode::And(a, b) | MuNode::Or(a, b) => {
            collect_boolean_atoms(f, *a, out);
            collect_boolean_atoms(f, *b, out);
        }
        _ => {}
    }
}

/// The leaf cells (`state` / `input`) in the combinational cone of `nid`.
fn combinational_leaves(file: &Btor2File, nid: Nid) -> HashSet<Nid> {
    let mut out = HashSet::new();
    let mut stack = vec![nid];
    let mut seen = HashSet::new();
    while let Some(n) = stack.pop() {
        if !seen.insert(n) {
            continue;
        }
        match file.lookup(n).map(|l| &l.node) {
            Some(Node::State { .. } | Node::Input { .. }) => {
                out.insert(n);
            }
            Some(Node::Op { args, .. }) => stack.extend(args.iter().map(|a| a.nid())),
            _ => {}
        }
    }
    out
}

/// Every node whose combinational cone contains `sig` (including `sig`).
fn dependents(file: &Btor2File, sig: Nid) -> HashSet<Nid> {
    let mut dep: HashSet<Nid> = HashSet::from([sig]);
    for l in &file.lines {
        if let Node::Op { args, .. } = &l.node
            && args.iter().any(|a| dep.contains(&a.nid()))
        {
            dep.insert(l.nid);
        }
    }
    dep
}

fn line(nid: Nid, node: Node, immediates: Vec<u32>) -> Line {
    Line {
        nid,
        node,
        immediates,
        source_line: 0,
    }
}

fn sort_for(w: u32, sorts: &mut HashMap<u32, Nid>, out: &mut Vec<Line>, next: &mut Nid) -> Nid {
    *sorts.entry(w).or_insert_with(|| {
        let n = *next;
        *next += 1;
        out.push(line(
            n,
            Node::Sort {
                sort: Sort::BitVec { width: w },
            },
            Vec::new(),
        ));
        n
    })
}

fn const_nid(
    w: u32,
    v: u64,
    consts: &mut HashMap<(u32, u64), Nid>,
    sorts: &mut HashMap<u32, Nid>,
    out: &mut Vec<Line>,
    next: &mut Nid,
) -> Nid {
    if let Some(n) = consts.get(&(w, v)) {
        return *n;
    }
    let s = sort_for(w, sorts, out, next);
    let n = *next;
    *next += 1;
    out.push(line(
        n,
        Node::Const {
            sort: s,
            value: ConstValue::Dec(v as i128),
        },
        Vec::new(),
    ));
    consts.insert((w, v), n);
    n
}

/// Give `nid` the symbol `name`: a fresh `uext … 0` alias (width-preserving), so a reused node
/// (a constant, an original node) can carry a property-specific name.
fn label(
    nid: Nid,
    name: &str,
    file: &Btor2File,
    widths: &HashMap<Nid, u32>,
    sorts: &mut HashMap<u32, Nid>,
    out: &mut Vec<Line>,
    next: &mut Nid,
) -> String {
    let w = width_of(nid, file, widths, out).unwrap_or(1);
    let s = sort_for(w, sorts, out, next);
    let n = *next;
    *next += 1;
    out.push(line(
        n,
        Node::Op {
            op: Op::Uext,
            sort: s,
            args: vec![Operand(nid)],
            symbol: Some(name.to_string()),
        },
        vec![0],
    ));
    name.to_string()
}

/// The width of `nid`, looking in the file and in the lines minted so far.
fn width_of(
    nid: Nid,
    file: &Btor2File,
    widths: &HashMap<Nid, u32>,
    minted: &[Line],
) -> Option<u32> {
    let sort = match file.lookup(nid).map(|l| &l.node) {
        Some(Node::State { sort, .. } | Node::Input { sort, .. } | Node::Const { sort, .. }) => {
            *sort
        }
        Some(Node::Op { sort, .. }) => *sort,
        _ => match &minted.iter().find(|l| l.nid == nid)?.node {
            Node::Const { sort, .. } | Node::Op { sort, .. } => *sort,
            _ => return None,
        },
    };
    widths.get(&sort).copied().or_else(|| {
        minted.iter().find_map(|l| match &l.node {
            Node::Sort {
                sort: Sort::BitVec { width },
            } if l.nid == sort => Some(*width),
            _ => None,
        })
    })
}

/// The value of a constant node, original or minted, when it fits in a `u64`.
fn const_value(nid: Nid, file: &Btor2File, minted: &[Line]) -> Option<u64> {
    if let Some(v) = super::bit_blast::resolve_btor2_constant(file, nid) {
        return Some(v);
    }
    match &minted.iter().find(|l| l.nid == nid)?.node {
        Node::Const {
            value: ConstValue::Dec(d),
            ..
        } => Some(*d as u64),
        _ => None,
    }
}

/// Copy the combinational cone of `root` with `sig` replaced by `k`, folding constants: the node
/// itself when it does not depend on `sig`; `k` for `sig`; otherwise a fresh node over the copied
/// operands, except that `eq` / `neq` of two constants and `ite` on a constant condition fold.
#[allow(clippy::too_many_arguments)]
fn copy_subst(
    file: &Btor2File,
    root: Nid,
    sig: Nid,
    k: Nid,
    dep: &HashSet<Nid>,
    consts: &mut HashMap<(u32, u64), Nid>,
    sorts: &mut HashMap<u32, Nid>,
    out: &mut Vec<Line>,
    next: &mut Nid,
    memo: &mut HashMap<Nid, Nid>,
) -> Nid {
    if root == sig {
        return k;
    }
    if !dep.contains(&root) {
        return root;
    }
    if let Some(m) = memo.get(&root) {
        return *m;
    }
    let Some(Node::Op { op, sort, args, .. }) = file.lookup(root).map(|l| l.node.clone()) else {
        return root;
    };
    let immediates = file
        .lookup(root)
        .map(|l| l.immediates.clone())
        .unwrap_or_default();
    let new_args: Vec<Operand> = args
        .iter()
        .map(|a| {
            let m = copy_subst(file, a.nid(), sig, k, dep, consts, sorts, out, next, memo);
            Operand(if a.is_negated() { -m } else { m })
        })
        .collect();
    let cv = |o: &Operand| -> Option<u64> {
        if o.is_negated() {
            return None;
        }
        const_value(o.nid(), file, out)
    };
    let folded: Option<Nid> = match op {
        Op::Eq | Op::Neq if new_args.len() == 2 => match (cv(&new_args[0]), cv(&new_args[1])) {
            (Some(a), Some(b)) => {
                let t = if op == Op::Eq { a == b } else { a != b };
                Some(const_nid(1, t as u64, consts, sorts, out, next))
            }
            _ => None,
        },
        Op::Ite if new_args.len() == 3 => match cv(&new_args[0]) {
            Some(c) => Some(if c & 1 == 1 { new_args[1] } else { new_args[2] }.nid())
                .filter(|_| !new_args[1].is_negated() && !new_args[2].is_negated()),
            None => None,
        },
        _ => None,
    };
    let result = match folded {
        Some(n) => n,
        None => {
            let n = *next;
            *next += 1;
            out.push(line(
                n,
                Node::Op {
                    op,
                    sort,
                    args: new_args,
                    symbol: None,
                },
                immediates,
            ));
            n
        }
    };
    memo.insert(root, result);
    result
}

fn rename_registers(expr: &PredicateExpr, renames: &HashMap<String, String>) -> PredicateExpr {
    let r = |s: &String| renames.get(s).cloned().unwrap_or_else(|| s.clone());
    match expr {
        PredicateExpr::Cmp {
            register,
            op,
            value,
        } => PredicateExpr::Cmp {
            register: r(register),
            op: *op,
            value: *value,
        },
        PredicateExpr::CmpReg { lhs, op, rhs } => PredicateExpr::CmpReg {
            lhs: r(lhs),
            op: *op,
            rhs: r(rhs),
        },
        PredicateExpr::CmpRegAddend {
            lhs,
            op,
            rhs,
            addend,
            width,
        } => PredicateExpr::CmpRegAddend {
            lhs: r(lhs),
            op: *op,
            rhs: r(rhs),
            addend: *addend,
            width: *width,
        },
        PredicateExpr::And(a, b) => PredicateExpr::And(
            Box::new(rename_registers(a, renames)),
            Box::new(rename_registers(b, renames)),
        ),
        PredicateExpr::Or(a, b) => PredicateExpr::Or(
            Box::new(rename_registers(a, renames)),
            Box::new(rename_registers(b, renames)),
        ),
        PredicateExpr::Not(a) => PredicateExpr::Not(Box::new(rename_registers(a, renames))),
        other => other.clone(),
    }
}

fn op_str(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "==",
        CmpOp::Ne => "!=",
        CmpOp::Lt => "<",
        CmpOp::Le => "<=",
        CmpOp::Gt => ">",
        CmpOp::Ge => ">=",
    }
}

/// The atom text the predicate parser reads back to the same expression; `None` for a form this
/// pass does not rewrite (`Select`).
pub(crate) fn render(expr: &PredicateExpr) -> Option<String> {
    Some(match expr {
        PredicateExpr::Cmp {
            register,
            op,
            value,
        } => format!("{register} {} {value}", op_str(*op)),
        PredicateExpr::CmpReg { lhs, op, rhs } => format!("{lhs} {} {rhs}", op_str(*op)),
        PredicateExpr::CmpRegAddend {
            lhs,
            op,
            rhs,
            addend,
            ..
        } => {
            format!("{lhs} {} {rhs} + {addend}", op_str(*op))
        }
        PredicateExpr::And(a, b) => format!("({}) && ({})", render(a)?, render(b)?),
        PredicateExpr::Or(a, b) => format!("({}) || ({})", render(a)?, render(b)?),
        PredicateExpr::Not(a) => format!("!({})", render(a)?),
        PredicateExpr::Select { .. } => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::symbolic_bitblast::{
        ExactSymbolicOptions, ExactVerdict, exact_symbolic_verdict,
        exact_symbolic_verdict_with_options,
    };

    /// mununu#629 — the consumer's shape: a 23-word (736-bit) packed vector crossed twice, a
    /// 23-arm address mux `rdata`, and `status_word` = word 4 read directly. `omit_merge` wires
    /// arm 4 to the UN-synchronised copy (`words`, "STATUS served as it crossed" — one of the two
    /// contrast twins the issue could not refute); `addr_is_state` registers the address instead
    /// of reading it as an input; `constrain` adds `constraint (addr != 4)`.
    ///
    /// The issue's other twin — flags SWAPPED, arm 4 wired to word 5 — is rewritten the same way
    /// but its refutation is `ws[191:160] == ws[159:128]`, an equality between MISALIGNED bits of
    /// one register, which is exponential under the engine's fixed interleaved order (measured:
    /// the arena exhausts after 60 s). That is the variable-order wall of
    /// `docs/design/bdd-variable-ordering.md`, not a cone question; recorded, not fixed here.
    fn fixture(omit_merge: bool, addr_is_state: bool, constrain: bool) -> String {
        let mut b = String::from(
            "1 sort bitvec 736\n2 input 1 rd_words\n3 state 1 words\n4 next 1 3 2\n\
             5 state 1 words_sync\n6 next 1 5 3\n7 sort bitvec 32\n8 sort bitvec 1\n9 sort bitvec 5\n",
        );
        let mut nid = if addr_is_state {
            b.push_str("10 state 9 addr\n11 input 9 addr_in\n12 next 9 10 11\n");
            13
        } else {
            b.push_str("10 input 9 addr\n");
            11
        };
        b.push_str(&format!("{nid} slice 7 5 159 128 status_word\n"));
        nid += 1;
        b.push_str(&format!("{nid} slice 7 3 159 128 word4_as_crossing\n"));
        let crossing = nid;
        nid += 1;
        let mut slices = Vec::new();
        for k in 0..23 {
            b.push_str(&format!("{nid} slice 7 5 {} {}\n", k * 32 + 31, k * 32));
            slices.push(nid);
            nid += 1;
        }
        let mut acc = slices[0];
        for (k, sl) in slices.iter().enumerate().skip(1) {
            let arm = if omit_merge && k == 4 { crossing } else { *sl };
            b.push_str(&format!("{nid} constd 9 {k}\n"));
            let kc = nid;
            nid += 1;
            b.push_str(&format!("{nid} eq 8 10 {kc}\n"));
            let is_k = nid;
            nid += 1;
            b.push_str(&format!("{nid} ite 7 {is_k} {arm} {acc}\n"));
            acc = nid;
            nid += 1;
        }
        b.push_str(&format!("{nid} uext 7 {acc} 0 rdata\n"));
        nid += 1;
        if constrain {
            b.push_str(&format!("{nid} constd 9 4\n"));
            let k4 = nid;
            nid += 1;
            b.push_str(&format!("{nid} eq 8 10 {k4}\n"));
            let is4 = nid;
            nid += 1;
            b.push_str(&format!("{nid} not 8 {is4}\n"));
            let not4 = nid;
            nid += 1;
            b.push_str(&format!("{nid} constraint {not4}\n"));
        }
        b
    }
    const PROP: &str = "nu X. ((!(addr == 4) || (rdata == status_word)) && [] X)";
    const AS_LIFTED: ExactSymbolicOptions = ExactSymbolicOptions {
        antecedent_shadow_enabled: true,
        signal_level_coi: false,
        antecedent_propagate_enabled: false,
    };

    /// The mux folds to the selected arm and the input antecedent is dropped: the property
    /// reads one 32-bit field of the record (96 bits over the two copies and the input).
    #[test]
    fn x629_the_mux_folds_to_the_selected_arm_and_the_input_antecedent_is_dropped() {
        let file = parser::parse(&fixture(false, false, false)).unwrap();
        let f = crate::mu_calculus::parser::parse(PROP).unwrap();
        let p = propagate(&file, &f).expect("the implication qualifies");
        assert_eq!(p.rewrites, vec![("addr".to_string(), 4, true)]);
        let atoms: Vec<&str> = p
            .formula
            .nodes()
            .iter()
            .filter_map(|n| match n {
                MuNode::Predicate(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            atoms.contains(&"rdata__addr_eq_4 == status_word"),
            "{atoms:?}"
        );
        // The root's body is the rewritten consequent, no `Or` over the antecedent left.
        let MuNode::Nu { body, .. } = p.formula.node(p.formula.root()) else {
            panic!()
        };
        let MuNode::And(l, _) = p.formula.node(*body) else {
            panic!()
        };
        assert!(
            matches!(p.formula.node(*l), MuNode::Predicate(t) if t == "rdata__addr_eq_4 == status_word")
        );
        let cone =
            crate::adapter::btor2::dep_graph::bit_cone(&p.file, &["rdata__addr_eq_4".to_string()])
                .expect("bit-vector cone");
        assert_eq!(
            crate::adapter::btor2::dep_graph::bit_cone_bits(&cone),
            96,
            "the one arm, both copies, the input"
        );
        assert!(
            crate::adapter::btor2::dep_graph::bit_cone(&file, &["rdata".to_string()])
                .map(|c| crate::adapter::btor2::dep_graph::bit_cone_bits(&c))
                .unwrap()
                > 2000,
            "the un-propagated mux reaches every word"
        );
    }

    /// The issue's measurement, decided: the correct design HOLDS, the merge-omitted twin is
    /// VIOLATED (a 2-valued refutation of a defect the abstracting engines left ⊥), and as
    /// lifted the exact engine refuses the input atom.
    #[test]
    fn x629_the_crossed_vector_mux_property_decides_both_ways() {
        let f = crate::mu_calculus::parser::parse(PROP).unwrap();
        let correct = fixture(false, false, false);
        let merge_omitted = fixture(true, false, false);
        assert_eq!(
            exact_symbolic_verdict(&correct, &f).expect("decides"),
            ExactVerdict::Holds
        );
        assert_eq!(
            exact_symbolic_verdict(&merge_omitted, &f).expect("decides"),
            ExactVerdict::Violated
        );
        let err = exact_symbolic_verdict_with_options(&correct, &f, &AS_LIFTED)
            .expect_err("as lifted: the antecedent pins a primary input");
        assert!(err.contains("primary input"), "{err}");
    }

    /// A registered address keeps its antecedent (a state atom) and still folds the mux: the
    /// property decides, where as lifted it abstains on the bit cap.
    #[test]
    fn x629_a_state_antecedent_stays_and_the_consequent_still_folds() {
        let f = crate::mu_calculus::parser::parse(PROP).unwrap();
        let design = fixture(false, true, false);
        let file = parser::parse(&design).unwrap();
        let p = propagate(&file, &f).unwrap();
        assert_eq!(p.rewrites, vec![("addr".to_string(), 4, false)]);
        assert!(
            p.formula
                .nodes()
                .iter()
                .any(|n| matches!(n, MuNode::Predicate(t) if t == "addr == 4"))
        );
        assert_eq!(
            exact_symbolic_verdict(&design, &f).expect("decides"),
            ExactVerdict::Holds
        );
        let err =
            exact_symbolic_verdict_with_options(&design, &f, &AS_LIFTED).expect_err("over the cap");
        assert!(err.contains("BIT CAP"), "{err}");
    }

    /// A constrained input keeps its antecedent (dropping it would be unsound for VIOLATED), so
    /// the engine's input-atom refusal stands.
    #[test]
    fn x629_a_constrained_input_antecedent_is_not_dropped() {
        let f = crate::mu_calculus::parser::parse(PROP).unwrap();
        let design = fixture(false, false, true);
        let file = parser::parse(&design).unwrap();
        let p = propagate(&file, &f).unwrap();
        assert_eq!(p.rewrites, vec![("addr".to_string(), 4, false)]);
        let err =
            exact_symbolic_verdict(&design, &f).expect_err("the antecedent still pins the input");
        assert!(err.contains("primary input"), "{err}");
    }

    /// Nothing to do: no implication, a non-equality antecedent, an unknown signal.
    #[test]
    fn x629_declines_what_it_cannot_rewrite() {
        let file = parser::parse(&fixture(false, false, false)).unwrap();
        for s in [
            "nu X. ((rdata == status_word) && [] X)",
            "nu X. ((!(addr != 4) || (rdata == status_word)) && [] X)",
            "nu X. ((!(nobody == 4) || (rdata == status_word)) && [] X)",
        ] {
            let f = crate::mu_calculus::parser::parse(s).unwrap();
            assert!(propagate(&file, &f).is_none(), "{s}");
        }
    }

    /// The atom renderer reads back to the same expression for every form it rewrites.
    #[test]
    fn x629_render_round_trips_through_the_parser() {
        for s in [
            "cnt == 3",
            "a != b",
            "a <= b + 2",
            "(a == 1) && (b >= 2)",
            "!(x < 4)",
            "((a == 1) || (b == 2)) && (c != 0)",
        ] {
            let e = parse_predicate_atom_bool(s).unwrap();
            let r = render(&e).unwrap();
            assert_eq!(parse_predicate_atom_bool(&r).unwrap(), e, "{s} -> {r}");
        }
    }
}
