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
//! stub, and that refusal stands. On the two-player verbs (mununu#661) only the UNCONTROLLABLE
//! inputs the atoms reach are lifted — the environment's choice for the next frame is exactly
//! the free `name$next` the rewrite introduces, so the game is unchanged; a CONTROLLABLE input
//! folded into the state would be fixed before the controller moves, which changes who wins,
//! so an atom reaching one stays refused by name.
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
    let mut input_names: HashSet<String> = file
        .inputs()
        .filter_map(|l| match &l.node {
            Node::Input {
                symbol: Some(s), ..
            } => Some(s.clone()),
            _ => None,
        })
        .collect();
    // mununu#1062 — a named next-less state (a cut net) is an input here too; mununu#1086 — so
    // is a symbol-less one, under the name the engines give it (`state_<nid>`).
    let free = free_states(file);
    input_names.extend(free.values().map(|(_, name)| name.clone()));
    let mut name_to_nid: HashMap<String, Nid> = collect_symbols(file)
        .into_iter()
        .map(|(nid, name)| (name, nid))
        .collect();
    for line in &file.lines {
        match &line.node {
            Node::Output {
                symbol: Some(s),
                signal,
            } => {
                name_to_nid.entry(s.clone()).or_insert(signal.nid());
            }
            // An internal `assign` wire is an Op symbol (`uext … 0 active`); `collect_symbols`
            // records those only as names for unsymbolled state cells. An atom over such a
            // wire reads its inputs through the wire — leave it out and the lift misses them
            // (t2's `active = (trig == 0)`: `en` lifted, `trig` not, a spurious VIOLATED).
            Node::Op {
                symbol: Some(s), ..
            } => {
                name_to_nid.entry(s.clone()).or_insert(line.nid);
            }
            _ => {}
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
            // mununu#1086 — `cone_inputs` reports NAMED cells only (its other reader, the
            // verify-auto seeder, seeds by name); the symbol-less next-less states the cone
            // reaches are collected here under their synthesized names.
            out.extend(anonymous_free_states_in_cone_of(file, nid, &free));
        }
    }
    out.into_iter().collect()
}

/// mununu#1062 — the state cells with no `next` line: `nid → (sort, name)`. Such a cell is free
/// at every step (a `--cutpoint` net, a black-boxed output), so for an atom that reads it it is
/// an input: the lift gives it `next = name$next`, a fresh free input, which makes it the
/// sampled cell of that input — the same product `(s, i)` the rewrite builds for an `input`
/// line, with no reader to redirect because every reader already reads the cell. Its `init`, if
/// it has one, stays; with none it is free at cycle 0 like every sampled input.
///
/// mununu#1086 — a cell with no symbol and no alias is named `state_<nid>`, the name
/// [`crate::adapter::btor2::parser::resolve_leaf_symbol`] gives it everywhere else (so the
/// engine's sampled-cell lookup finds it after the lift). Before this such a cell was skipped:
/// neither sampled nor refused, the exact engine read one value of it in the atom and another
/// in the transition. A design that already carries that name on another cell keeps the cell
/// unsampled, said on the log — the collision is the design's, and the alternative (two cells
/// under one name) is worse.
fn free_states(file: &Btor2File) -> HashMap<Nid, (Nid, String)> {
    let has_next: HashSet<Nid> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Next { state, .. } => Some(*state),
            _ => None,
        })
        .collect();
    let symbols = collect_symbols(file);
    let taken: HashSet<&String> = symbols.values().collect();
    file.lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::State { sort, .. } if !has_next.contains(&l.nid) => {
                let name = match symbols.get(&l.nid) {
                    Some(name) => name.clone(),
                    None => {
                        let name = format!("state_{}", l.nid);
                        if taken.contains(&name) {
                            tracing::warn!(
                                nid = l.nid,
                                "a symbol-less state with no `next` cannot be sampled: the \
                                 design already names another cell `{name}`; an atom reading \
                                 it is read per step (mununu#1086)"
                            );
                            return None;
                        }
                        name
                    }
                };
                Some((l.nid, (*sort, name)))
            }
            _ => None,
        })
        .collect()
}

/// mununu#1086 — the symbol-less next-less states (under their synthesized names) in the
/// combinational cone of `start`, stopping at every register as
/// [`crate::adapter::btor2::parser::cone_inputs`] does. What the exact engine's fan-in guard
/// adds to `cone_inputs` when the lift is opted out, so such an atom is refused rather than
/// read per step. Sorted and deduplicated; empty after the lift (the cell has a `next` then).
pub(crate) fn anonymous_free_states_in_cone(file: &Btor2File, start: Nid) -> Vec<String> {
    anonymous_free_states_in_cone_of(file, start, &free_states(file))
}

fn anonymous_free_states_in_cone_of(
    file: &Btor2File,
    start: Nid,
    free: &HashMap<Nid, (Nid, String)>,
) -> Vec<String> {
    let symbols = collect_symbols(file);
    let mut seen: HashSet<Nid> = HashSet::new();
    let mut out: BTreeSet<String> = BTreeSet::new();
    let mut work: Vec<Nid> = vec![start];
    while let Some(nid) = work.pop() {
        if !seen.insert(nid) {
            continue;
        }
        let Some(line) = file.lookup(nid) else {
            continue;
        };
        match &line.node {
            Node::State { .. } => {
                if !symbols.contains_key(&nid)
                    && let Some((_, name)) = free.get(&nid)
                {
                    out.insert(name.clone());
                }
            }
            Node::Input { .. } | Node::Const { .. } | Node::Sort { .. } => {}
            Node::Op { args, .. } => work.extend(args.iter().map(Operand::nid)),
            Node::Init { value, .. } | Node::Next { value, .. } => work.push(value.nid()),
            Node::Bad { signal }
            | Node::Constraint { signal }
            | Node::Fair { signal }
            | Node::Output { signal, .. } => work.push(signal.nid()),
            Node::Justice { signals } => work.extend(signals.iter().map(Operand::nid)),
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

/// [`lift_atom_inputs`] minus an exclusion set (mununu#661 — the two-player verbs pass their
/// controllable inputs): the atoms' inputs NOT in `exclude` are lifted; an excluded input the
/// atoms reach is left as a per-step input, so the engine's input-atom refusal still names it.
/// `None` when nothing is left to lift.
pub fn lift_atom_inputs_excluding(
    file: &Btor2File,
    atom_registers: &HashSet<String>,
    exclude: &HashSet<String>,
) -> Option<InputStateLift> {
    let lifted: Vec<String> = atom_inputs(file, atom_registers)
        .into_iter()
        .filter(|name| !exclude.contains(name))
        .collect();
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
    // mununu#1062 — the wanted next-less states: closed below with a fresh next-cycle input.
    let mut to_close: Vec<(Nid, Nid, String)> = free_states(file)
        .into_iter()
        .filter(|(_, (_, name))| wanted.contains(name.as_str()))
        .map(|(nid, (sort, name))| (nid, sort, name))
        .collect();
    to_close.sort();
    if to_lift.is_empty() && to_close.is_empty() {
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
    // mununu#1062 — `next f = f$next` for each wanted next-less state `f`.
    for (state, sort, name) in to_close {
        let input = next_nid;
        lines.push(Line {
            nid: input,
            node: Node::Input {
                sort,
                symbol: Some(format!("{name}$next")),
            },
            immediates: Vec::new(),
            source_line: 0,
        });
        lines.push(Line {
            nid: input + 1,
            node: Node::Next {
                sort,
                state,
                value: Operand(input),
            },
            immediates: Vec::new(),
            source_line: 0,
        });
        next_nid += 2;
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

    /// mununu#1062 — `accept = in_valid & (!v | mergeable)`, `v' = accept`, and `mergeable` is a
    /// cut net: a state with no `next`.
    const CUT_IN_ATOM_CONE: &str = "1 sort bitvec 1\n2 input 1 in_valid\n3 state 1 mergeable\n\
4 state 1 v\n5 zero 1\n6 init 1 4 5\n7 not 1 4\n8 or 1 7 3\n9 and 1 2 8\n10 output 9 accept\n\
11 next 1 4 9\n";

    /// mununu#1062 — a cut net an atom reads is one of the atom's inputs, and the lift closes it
    /// with a fresh next-cycle input so the atom and the transition read one value per cycle.
    #[test]
    fn x1062_a_cut_net_in_an_atoms_cone_is_sampled_like_an_input() {
        let file = parse(CUT_IN_ATOM_CONE).expect("parses");
        let regs: HashSet<String> = ["accept".to_string()].into_iter().collect();
        assert_eq!(atom_inputs(&file, &regs), vec!["in_valid", "mergeable"]);
        let lift = lift_atom_inputs(&file, &regs).expect("lifts");
        assert_eq!(lift.lifted, vec!["in_valid", "mergeable"]);
        let symbols = collect_symbols(&lift.file);
        let next_of_cut = lift
            .file
            .lines
            .iter()
            .find_map(|l| match &l.node {
                Node::Next {
                    state: 3, value, ..
                } => Some(value.nid()),
                _ => None,
            })
            .expect("the cut net has a `next` after the lift");
        assert_eq!(
            symbols.get(&next_of_cut).map(String::as_str),
            Some("mergeable$next")
        );
        // After the lift the atom reads registers only.
        let accept = 9;
        assert!(cone_inputs(&lift.file, accept).is_empty());
    }

    /// mununu#1086 — the same cut net with NO symbol (`3 state 1`): nothing names it, so it is
    /// sampled under the engines' synthesized name `state_3`, closed with `state_3$next`, and
    /// the atom's cone reaches no free cell afterwards. Before this the cell was skipped by
    /// `free_states` and by the cone walk alike: neither sampled nor refused.
    #[test]
    fn x1086_an_anonymous_cut_net_in_an_atoms_cone_is_sampled_under_its_synthesized_name() {
        const ANONYMOUS_CUT: &str = "1 sort bitvec 1\n2 input 1 in_valid\n3 state 1\n\
4 state 1 v\n5 zero 1\n6 init 1 4 5\n7 not 1 4\n8 or 1 7 3\n9 and 1 2 8\n10 output 9 accept\n\
11 next 1 4 9\n";
        let file = parse(ANONYMOUS_CUT).expect("parses");
        assert!(
            !collect_symbols(&file).contains_key(&3),
            "the fixture's cut net has no name from any pass"
        );
        assert_eq!(anonymous_free_states_in_cone(&file, 9), vec!["state_3"]);
        assert!(
            cone_inputs(&file, 9) == vec!["in_valid"],
            "the named walk still reports the named input only: {:?}",
            cone_inputs(&file, 9)
        );
        let regs: HashSet<String> = ["accept".to_string()].into_iter().collect();
        assert_eq!(atom_inputs(&file, &regs), vec!["in_valid", "state_3"]);
        let lift = lift_atom_inputs(&file, &regs).expect("lifts");
        assert_eq!(lift.lifted, vec!["in_valid", "state_3"]);
        let symbols = collect_symbols(&lift.file);
        let next_of_cut = lift
            .file
            .lines
            .iter()
            .find_map(|l| match &l.node {
                Node::Next {
                    state: 3, value, ..
                } => Some(value.nid()),
                _ => None,
            })
            .expect("the anonymous cut net has a `next` after the lift");
        assert_eq!(
            symbols.get(&next_of_cut).map(String::as_str),
            Some("state_3$next")
        );
        assert!(cone_inputs(&lift.file, 9).is_empty());
        assert!(anonymous_free_states_in_cone(&lift.file, 9).is_empty());
        // A design that already names another cell `state_3` keeps the anonymous cell as is.
        const COLLISION: &str = "1 sort bitvec 1\n2 input 1 state_3\n3 state 1\n\
4 state 1 v\n5 zero 1\n6 init 1 4 5\n7 not 1 4\n8 or 1 7 3\n9 and 1 2 8\n10 output 9 accept\n\
11 next 1 4 9\n";
        let file = parse(COLLISION).expect("parses");
        assert_eq!(
            atom_inputs(&file, &regs),
            vec!["state_3"],
            "the input, not the cell"
        );
        assert!(anonymous_free_states_in_cone(&file, 9).is_empty());
    }

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

    /// An internal wire named by an Op symbol (`assign both = req & ready`), not an output:
    /// `collect_symbols` does not record it, so the lift (and the engine's refusal) must read
    /// Op symbols too — left out, `t3`'s `active = (trig == 0)` antecedent read a per-step
    /// input unrefused and refuted a true property.
    #[test]
    fn x656_an_op_symbol_wire_atom_lifts_the_inputs_it_reads() {
        let with_wire = format!("{MEALY}13 uext 1 8 0 both\n");
        let file = parse(&with_wire).expect("parses");
        let regs: HashSet<String> = ["both".to_string()].into_iter().collect();
        assert_eq!(atom_inputs(&file, &regs), vec!["ready", "req"]);
    }

    /// mununu#677 — a one-shot `|=>` whose antecedent is an input-derived WIRE. `r' = in`, and
    /// `w` is `in` through a named wire, so `w |=> (r == 1)` HOLDS: the register one cycle after
    /// `w` is exactly `w`'s input. The #476 shadow rewrite renamed `w` to a register sampling it
    /// while keeping the `[]`, which checks the consequent one cycle late (`r` two cycles after
    /// `w`, where `in` is free again) and refuted it; with the lift the atom reads the sampled
    /// `in` and the property decides as stated. The same design under `nu X. ((!(in == 1) ||
    /// [] (r == 1)) && [] X)` — the bare input, never shadowed — is the control.
    #[test]
    fn x677_a_one_shot_pipe_implication_on_an_input_derived_wire_holds() {
        use crate::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};
        // `w` as an OUTPUT symbol (what yosys emits for a module port) and the atoms spelled
        // BARE (`w`, not `w == 1`), as the SVA translator spells a 1-bit signal — the shadow
        // synthesis resolves an atom by that string. `r' = in & acc` for the partial case.
        const DELAY: &str = "1 sort bitvec 1\n2 input 1 in\n3 state 1 r\n4 zero 1\n5 init 1 3 4\n\
                             7 uext 1 2 0\n8 output 7 w\n9 input 1 acc\n10 and 1 2 9\n\
                             11 next 1 3 10\n";
        let f = |s: &str| crate::mu_calculus::parser::parse(s).expect("formula");
        assert_eq!(
            exact_symbolic_verdict(DELAY, &f("nu X. ((!(in && acc) || [] r) && [] X)"))
                .expect("decides"),
            ExactVerdict::Holds,
            "the bare-input antecedent (control: never shadowed, lifted)"
        );
        assert_eq!(
            exact_symbolic_verdict(DELAY, &f("nu X. ((!(w && acc) || [] r) && [] X)"))
                .expect("decides"),
            ExactVerdict::Holds,
            "the wire antecedent: r one cycle after (w && acc) is exactly w & acc"
        );
        // One-shot in the other direction: `(w && acc) |=> !r` is refutable, so no rewrite may
        // turn it into HOLDS.
        assert_eq!(
            exact_symbolic_verdict(DELAY, &f("nu X. ((!(w && acc) || [] (!(r))) && [] X)"))
                .expect("decides"),
            ExactVerdict::Violated
        );
    }

    /// mununu#661 — the exclusion set: a controllable input the atoms reach is NOT lifted (it
    /// stays a per-step input, by name), the environment one is.
    #[test]
    fn x661_lift_atom_inputs_excluding_keeps_a_controllable_input_per_step() {
        const ARBITER: &str = "1 sort bitvec 1\n2 input 1 grant\n3 input 1 req\n4 state 1 q\n\
                               5 zero 1\n6 init 1 4 5\n7 next 1 4 2\n9 and 1 4 3 a\n\
                               11 uext 1 2 0 g\n";
        let file = crate::adapter::btor2::parser::parse(ARBITER).expect("parse");
        let atoms: HashSet<String> = ["a".to_string(), "g".to_string()].into_iter().collect();
        let ctrl: HashSet<String> = ["grant".to_string()].into_iter().collect();
        let lift = lift_atom_inputs_excluding(&file, &atoms, &ctrl).expect("req is lifted");
        assert_eq!(lift.lifted, vec!["req".to_string()]);
        let names: Vec<String> = lift
            .file
            .inputs()
            .filter_map(|l| match &l.node {
                Node::Input {
                    symbol: Some(s), ..
                } => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert!(
            names.contains(&"grant".to_string()),
            "grant stays a per-step input: {names:?}"
        );
        assert!(
            names.contains(&"req$next".to_string()),
            "req became the next-frame choice: {names:?}"
        );
        assert!(
            lift_atom_inputs_excluding(&file, &["g".to_string()].into_iter().collect(), &ctrl)
                .is_none(),
            "nothing to lift when the only input reached is controllable"
        );
    }

    /// mununu#757 — a property that asks for SOME trace reads the cycle-0 input existentially.
    /// The lift samples the cycle-0 input into the initial state, and the exact engine read a
    /// verdict as `init ⊆ sat`, quantifying that input universally: a latch that breaks for good
    /// on a bad cycle-0 input (`ok = prev ∧ a`, the shape of the assumption monitor
    /// `mununu_assume_ok`) made `EF ok` fail at the initial states with `a = 0`, so a `cover`
    /// relativized to an input-reading `assume` came back UNREACHABLE although the environment
    /// can choose `a = 1`. A property over EVERY trace keeps the universal reading.
    #[test]
    fn an_existential_property_reads_the_sampled_cycle0_input_existentially() {
        use crate::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};
        const LATCH: &str = "1 sort bitvec 1\n2 input 1 a\n3 state 1 prev_ok\n4 one 1\n\
                             5 init 1 3 4\n6 and 1 3 2 ok\n7 next 1 3 6\n";
        let verdict = |f: &str| {
            exact_symbolic_verdict(
                LATCH,
                &crate::mu_calculus::parser::parse(f).expect("formula"),
            )
            .expect("decides")
        };
        assert_eq!(
            verdict("mu X. (ok || <> X)"),
            ExactVerdict::Holds,
            "`EF ok`: the environment picks `a = 1` at cycle 0"
        );
        assert_eq!(
            verdict("mu X. ((!(ok)) || <> X)"),
            ExactVerdict::Holds,
            "`EF !ok`: and `a = 0` breaks it"
        );
        assert_eq!(
            verdict("nu X. (ok && [] X)"),
            ExactVerdict::Violated,
            "`AG ok` quantifies the cycle-0 input universally"
        );
        assert_eq!(
            verdict("(!(mu X. ((!(ok)) || <> X)))"),
            ExactVerdict::Violated,
            "`!EF !ok` is `AG ok` — the negation flips the first modality to universal"
        );
    }
}
