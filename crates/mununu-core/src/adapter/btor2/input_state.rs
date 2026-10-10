//! mununu#656 — **inputs in the state**: the exact engine's model for a property that reads a
//! primary input in the same cycle it is chosen.
//!
//! The exact-symbolic engine's states are register valuations; an input exists only on a
//! transition, quantified away inside each `<>` / `[]` step. An atom over an input, or over a
//! signal combinationally driven by one (`gnt_o = req_i & sel(mask_q) & ready_i`), therefore
//! has no truth value *at a state*, and the engine refused it rather than pick a convention
//! that decouples an antecedent from its consequent ("atom `gnt_o` is combinationally driven
//! by primary input(s)"). OpenTitan's two round-robin arbiters sat on that refusal.
//!
//! The standard remedy — what every SVA checker's sampling semantics does — is to make the
//! state the pair `(s, i)`: the registers together with the input sampled in that cycle. This
//! module performs it as a BTOR2 rewrite, so every engine downstream sees an ordinary design:
//!
//! - for each lifted input `i` (sort `σ`, symbol `name`) a new state cell `i_s : σ` carries the
//!   symbol `name`, so the property's atoms resolve to it;
//! - the input line keeps its NID and becomes the **next-cycle** choice `name$next`;
//! - every reader of `i` — register `next` functions, outputs, `bad` / `constraint` / `fair` —
//!   is redirected to `i_s`, so the whole design reads the sampled value, as the RTL does;
//! - `next i_s = i` (the environment picks the next cycle's input freely) and `i_s` has no
//!   `init` (the environment is free at cycle 0, the mununu#505 / #579 reading).
//!
//! The result is the product `(s, i) → (δ(s, i), i')`: trace-equivalent to the original on the
//! registers, and the atoms are state predicates. A same-cycle implication's antecedent and
//! consequent read the SAME `i`; a `|=>` consequent reads the next state's `i'` — a fresh
//! choice — which is the intended semantics, and exactly what the per-atom quantification the
//! refusal guards against could not express. Definite verdicts transfer at every alternation
//! depth (nothing is abstracted: this is the concrete product, bit-blasted exactly).
//!
//! **Scope.** Only the named primary inputs the property's atoms reach are lifted — the rest
//! stay quantified per step, so the bit cap moves by the lifted width (twice: the sampled
//! cell and the fresh next input both enter the cone). Anonymous inputs (black-boxed
//! submodule outputs, undriven nets) are NOT lifted: a verdict over them would read a chaotic
//! stub, and that refusal stands. The two-player verbs do not use this rewrite: a controllable
//! input folded into the state is fixed before the controller moves, which changes who wins.
//!
//! engine: `exact-symbolic` (full-state ROBDD, bit-blast μ-fixpoint) over the input-extended
//! state. Differential switch: `MUNUNU_NO_INPUT_STATE=1` / `--no-input-state` /
//! `no_input_state: true` restore the refusal.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::adapter::btor2::ast::{Btor2File, Line, Nid, Node, Operand};
use crate::adapter::btor2::parser::{collect_symbols, cone_inputs};

/// The rewritten design and the inputs it lifted (sorted, by symbol).
pub struct InputStateLift {
    pub file: Btor2File,
    pub lifted: Vec<String>,
}

/// The named primary inputs the atoms read: an atom over an input itself, or over a signal
/// whose combinational cone reaches one (`cone_inputs`). Sorted and deduplicated.
pub fn atom_inputs(file: &Btor2File, atom_registers: &HashSet<String>) -> Vec<String> {
    let input_names: HashSet<String> = file
        .inputs()
        .filter_map(|l| match &l.node {
            Node::Input {
                symbol: Some(s), ..
            } => Some(s.clone()),
            _ => None,
        })
        .collect();
    let mut name_to_nid: HashMap<String, Nid> = collect_symbols(file)
        .into_iter()
        .map(|(nid, name)| (name, nid))
        .collect();
    for line in &file.lines {
        if let Node::Output {
            symbol: Some(s),
            signal,
        } = &line.node
        {
            name_to_nid.entry(s.clone()).or_insert(signal.nid());
        }
    }
    let mut out: BTreeSet<String> = BTreeSet::new();
    for reg in atom_registers {
        if input_names.contains(reg) {
            out.insert(reg.clone());
            continue;
        }
        if let Some(&nid) = name_to_nid.get(reg) {
            out.extend(cone_inputs(file, nid));
        }
    }
    out.into_iter().collect()
}

/// Lift the atoms' inputs into the state (see the module doc). `None` when the atoms read no
/// named primary input — the design is returned untouched by the caller.
pub fn lift_atom_inputs(
    file: &Btor2File,
    atom_registers: &HashSet<String>,
) -> Option<InputStateLift> {
    let lifted = atom_inputs(file, atom_registers);
    if lifted.is_empty() {
        return None;
    }
    Some(InputStateLift {
        file: lift_inputs(file, &lifted),
        lifted,
    })
}

/// The rewrite itself, for the named inputs given (unknown names are ignored).
pub fn lift_inputs(file: &Btor2File, inputs: &[String]) -> Btor2File {
    let wanted: HashSet<&str> = inputs.iter().map(String::as_str).collect();
    // input NID → (its sort, its symbol), for the inputs to lift.
    let mut to_lift: HashMap<Nid, (Nid, String)> = HashMap::new();
    for l in file.inputs() {
        if let Node::Input {
            sort,
            symbol: Some(s),
        } = &l.node
            && wanted.contains(s.as_str())
        {
            to_lift.insert(l.nid, (*sort, s.clone()));
        }
    }
    if to_lift.is_empty() {
        return file.clone();
    }
    let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    // input NID → the sampled state cell every reader is redirected to.
    let mut sampled: HashMap<Nid, Nid> = HashMap::new();
    for nid in to_lift.keys() {
        sampled.insert(*nid, next_nid);
        next_nid += 1;
    }
    let redirect = |o: &Operand| -> Operand {
        match sampled.get(&o.nid()) {
            Some(s) => Operand(if o.is_negated() { -*s } else { *s }),
            None => *o,
        }
    };

    let mut lines: Vec<Line> = Vec::with_capacity(file.lines.len() + 2 * to_lift.len());
    for line in &file.lines {
        let node = match &line.node {
            Node::Input { sort, .. } if to_lift.contains_key(&line.nid) => {
                let (_, name) = &to_lift[&line.nid];
                // The input line stays (same NID) as the NEXT-cycle choice; the sampled cell
                // takes the symbol, right after it, so every later reader can see it.
                lines.push(Line {
                    nid: line.nid,
                    node: Node::Input {
                        sort: *sort,
                        symbol: Some(format!("{name}$next")),
                    },
                    immediates: Vec::new(),
                    source_line: line.source_line,
                });
                lines.push(Line {
                    nid: sampled[&line.nid],
                    node: Node::State {
                        sort: *sort,
                        symbol: Some(name.clone()),
                    },
                    immediates: Vec::new(),
                    source_line: line.source_line,
                });
                continue;
            }
            Node::Op {
                sort,
                op,
                args,
                symbol,
            } => Node::Op {
                sort: *sort,
                op: *op,
                args: args.iter().map(redirect).collect(),
                symbol: symbol.clone(),
            },
            Node::Init { sort, state, value } => Node::Init {
                sort: *sort,
                state: *state,
                value: redirect(value),
            },
            Node::Next { sort, state, value } => Node::Next {
                sort: *sort,
                state: *state,
                value: redirect(value),
            },
            Node::Bad { signal } => Node::Bad {
                signal: redirect(signal),
            },
            Node::Constraint { signal } => Node::Constraint {
                signal: redirect(signal),
            },
            Node::Fair { signal } => Node::Fair {
                signal: redirect(signal),
            },
            Node::Output { symbol, signal } => Node::Output {
                symbol: symbol.clone(),
                signal: redirect(signal),
            },
            Node::Justice { signals } => Node::Justice {
                signals: signals.iter().map(redirect).collect(),
            },
            other => other.clone(),
        };
        lines.push(Line {
            nid: line.nid,
            node,
            immediates: line.immediates.clone(),
            source_line: line.source_line,
        });
    }
    // `next i_s = i`: the environment's choice for the next cycle. No `init`: free at cycle 0.
    let mut order: Vec<(&Nid, &Nid)> = sampled.iter().collect();
    order.sort();
    for (input, cell) in order {
        let (sort, _) = &to_lift[input];
        lines.push(Line {
            nid: next_nid,
            node: Node::Next {
                sort: *sort,
                state: *cell,
                value: Operand(*input),
            },
            immediates: Vec::new(),
            source_line: 0,
        });
        next_nid += 1;
    }
    let by_nid = lines.iter().enumerate().map(|(i, l)| (l.nid, i)).collect();
    Btor2File { lines, by_nid }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::parser::parse;

    /// A one-register Mealy arbiter in six lines: `gnt = req & ready & ~busy_q`, `busy_q`
    /// toggles on `req`. `gnt` is combinational of two inputs and one register.
    const MEALY: &str = "1 sort bitvec 1\n2 input 1 req\n3 input 1 ready\n4 state 1 busy_q\n\
5 zero 1\n6 init 1 4 5\n7 not 1 4\n8 and 1 2 3\n9 and 1 8 7\n10 output 9 gnt\n\
11 xor 1 4 2\n12 next 1 4 11\n";

    #[test]
    fn x656_the_atoms_inputs_are_the_named_inputs_in_their_cone() {
        let file = parse(MEALY).expect("parses");
        let regs: HashSet<String> = ["gnt".to_string()].into_iter().collect();
        assert_eq!(atom_inputs(&file, &regs), vec!["ready", "req"]);
        let regs: HashSet<String> = ["busy_q".to_string()].into_iter().collect();
        assert!(
            atom_inputs(&file, &regs).is_empty(),
            "a register atom reads no input"
        );
        let regs: HashSet<String> = ["req".to_string()].into_iter().collect();
        assert_eq!(
            atom_inputs(&file, &regs),
            vec!["req"],
            "an atom over the input itself"
        );
    }

    /// The rewrite: the input keeps its NID as `name$next`, a state cell takes `name`, every
    /// reader (the `and`, the register's `next`) reads the cell, and `next cell = input`.
    #[test]
    fn x656_lifting_an_input_makes_it_a_sampled_state_cell_with_a_free_next() {
        let file = parse(MEALY).expect("parses");
        let lifted = lift_inputs(&file, &["req".to_string()]);
        let syms = collect_symbols(&lifted);
        let cell = lifted
            .lines
            .iter()
            .find(|l| matches!(&l.node, Node::State { symbol: Some(s), .. } if s == "req"))
            .expect("a state cell named `req`")
            .nid;
        assert!(
            lifted.lines.iter().any(
                |l| matches!(&l.node, Node::Input { symbol: Some(s), .. } if s == "req$next")
                    && l.nid == 2
            ),
            "the input line keeps NID 2 as the next-cycle choice"
        );
        // Readers: `8 and 1 2 3` now reads the cell; `11 xor 1 4 2` too.
        for nid in [8, 11] {
            let Node::Op { args, .. } = &lifted.lookup(nid).unwrap().node else {
                panic!("op {nid}")
            };
            assert!(
                args.iter().any(|a| a.nid() == cell),
                "op {nid} reads the cell"
            );
            assert!(
                args.iter().all(|a| a.nid() != 2),
                "op {nid} no longer reads the input"
            );
        }
        assert!(
            lifted.lines.iter().any(|l| matches!(&l.node,
                Node::Next { state, value, .. } if *state == cell && value.nid() == 2)),
            "next cell = the fresh input"
        );
        assert!(
            !lifted
                .lines
                .iter()
                .any(|l| matches!(&l.node, Node::Init { state, .. } if *state == cell)),
            "the cell has no init: free at cycle 0"
        );
        assert_eq!(syms.get(&cell).map(String::as_str), Some("req"));
        assert_eq!(
            lifted.by_nid.len(),
            lifted.lines.len(),
            "the index is rebuilt"
        );
        // Untouched when nothing matches.
        assert_eq!(
            lift_inputs(&file, &["nope".to_string()]).lines.len(),
            file.lines.len()
        );
    }

    /// The engine on the product. `gnt = req & ready & ~busy_q`: from every state some next
    /// input clears the grant (`AG EF`), the grant is not always clear (`AG`), and the same-cycle
    /// coupling `gnt → req` HOLDS — the atom and the antecedent read the SAME sampled input,
    /// which per-atom quantification could not guarantee.
    #[test]
    fn x656_an_input_driven_atom_decides_on_the_product_and_keeps_same_cycle_coupling() {
        use crate::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};
        let f = |s: &str| crate::mu_calculus::parser::parse(s).expect("formula");
        let v = |s: &str| exact_symbolic_verdict(MEALY, &f(s)).expect("decides");
        assert_eq!(
            v("nu Y. ((mu X. ((gnt == 0) or <> X)) and [] Y)"),
            ExactVerdict::Holds
        );
        assert_eq!(v("nu Y. ((gnt == 0) and [] Y)"), ExactVerdict::Violated);
        assert_eq!(
            v("nu Y. (((gnt == 0) or (req == 1)) and [] Y)"),
            ExactVerdict::Holds
        );
        // `req == 1` now does NOT fix the next input: the register toggles, the grant may stay.
        assert_eq!(
            v("nu Y. (((req == 0) or [] (busy_q == 1)) and [] Y)"),
            ExactVerdict::Violated
        );
    }

    /// A `constraint` over an input is a state predicate on the product, and BTOR2 requires
    /// it at frame 0 too: an initial state with the sampled input at a forbidden value starts
    /// no valid trace and is not read. `constraint !req` makes `AG (req == 0)` HOLD — read at
    /// every initial state regardless of legality it would be a spurious VIOLATED — while the
    /// unconstrained design refutes it.
    #[test]
    fn x656_a_constraint_over_a_lifted_input_filters_the_initial_frame() {
        use crate::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};
        let f = |s: &str| crate::mu_calculus::parser::parse(s).expect("formula");
        let constrained = format!("{MEALY}13 not 1 2\n14 constraint 13\n");
        assert_eq!(
            exact_symbolic_verdict(&constrained, &f("nu Y. ((req == 0) and [] Y)")).unwrap(),
            ExactVerdict::Holds
        );
        assert_eq!(
            exact_symbolic_verdict(MEALY, &f("nu Y. ((req == 0) and [] Y)")).unwrap(),
            ExactVerdict::Violated
        );
    }
}
