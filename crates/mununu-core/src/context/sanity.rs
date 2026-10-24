//! `context eval --sanity`: structural checks a hand-authored model should pass before any
//! property is trusted.
//!
//! > Source of truth: [`check`] — surface: (CLI+API+UI), `context eval --sanity` and
//! > `sanity: true` on `POST /api/v1/context/verify`.
//!
//! Every check is an explicit graph computation over the realized automaton or composition, so it
//! is exact and linear in its size:
//!
//! - **deadlocks** — reachable states with no outgoing transition, each with a shortest
//!   initial → deadlock path. A composition makes them for reasons no single member shows: a
//!   synchronisation vector no partner offers (mununu#570), a shared label one side never takes,
//!   a guard that disables the last move. An intended terminal state (`Done`) is listed too; a
//!   property such as `no_deadlock` decides whether it is a defect.
//! - **unreachable states** — declared states no path from an initial state reaches (a single
//!   automaton; a composition's product has only reachable states by construction, and its
//!   member states are listed when no reachable product state carries them).
//! - **labels that never fire** — alphabet labels no reachable transition carries.
//! - **internal livelocks** — reachable cycles made of internal labels only (Tarjan SCCs over
//!   the `internal { }` edges): the system can spin forever without an observable step.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use serde::Serialize;

use crate::clts::{Clts, DefaultLabelIdx, DefaultStateIdx, StateId};

/// The findings of [`check`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SanityReport {
    pub automaton: String,
    pub reachable_states: usize,
    /// Reachable states with no outgoing transition, each with a shortest path from an initial
    /// state (`[Init, -label->, …]` as alternating state and label names).
    pub deadlocks: Vec<SanityPath>,
    pub unreachable_states: Vec<String>,
    pub labels_never_fired: Vec<String>,
    /// Each a set of states that can cycle forever on internal labels alone.
    pub internal_livelocks: Vec<Vec<String>>,
}

/// A path through the model: `states[0] -labels[0]-> states[1] …`. Serialized as a finite
/// [`Trace`](crate::trace::Trace).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SanityPath {
    pub states: Vec<String>,
    pub labels: Vec<String>,
}

impl Serialize for SanityPath {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        crate::trace::Trace::from_states(&self.states, &self.labels, None).serialize(s)
    }
}

impl SanityReport {
    /// No deadlock, no unreachable state, no dead label, no livelock.
    pub fn is_clean(&self) -> bool {
        self.deadlocks.is_empty()
            && self.unreachable_states.is_empty()
            && self.labels_never_fired.is_empty()
            && self.internal_livelocks.is_empty()
    }
}

type S = DefaultStateIdx;
type L = DefaultLabelIdx;

fn name(clts: &Clts<S, L>, s: StateId<S>) -> String {
    clts.state_name(s).unwrap_or("state").to_string()
}

fn labels_of(clts: &Clts<S, L>, t: &crate::clts::Transition<S, L>) -> String {
    let mut ls: Vec<String> = t
        .labels()
        .iter()
        .filter_map(|l| clts.label_payload(*l))
        .flat_map(|p| p.iter().cloned())
        .collect();
    ls.sort();
    ls.dedup();
    match ls.len() {
        0 => "epsilon".to_string(),
        1 => ls.remove(0),
        _ => format!("{{{}}}", ls.join(", ")),
    }
}

/// Run every structural check on `clts`. `declared_labels` are the labels the source declares
/// (an alphabet entry no transition uses never reaches the CLTS's own alphabet). `members` maps a
/// composition to its member automata, in product-name order (`a|b` names a state of members
/// `[A, B]`), for the member-level unreachable-state check; empty for an automaton.
pub fn check(
    automaton: &str,
    clts: &Clts<S, L>,
    declared_labels: &[String],
    members: &[(String, &Clts<S, L>)],
) -> SanityReport {
    // Breadth-first search from the initial states, keeping one parent edge per state.
    let n = clts.state_count();
    let mut seen = vec![false; n];
    let mut parent: HashMap<StateId<S>, (StateId<S>, String)> = HashMap::new();
    let mut queue: VecDeque<StateId<S>> = VecDeque::new();
    let mut initials: Vec<StateId<S>> = clts.initial_states().iter().copied().collect();
    initials.sort_by_key(|s| s.index());
    for s in &initials {
        seen[s.index()] = true;
        queue.push_back(*s);
    }
    let mut fired: BTreeSet<String> = BTreeSet::new();
    let mut reachable: Vec<StateId<S>> = Vec::new();
    while let Some(s) = queue.pop_front() {
        reachable.push(s);
        for t in clts.outgoing(s) {
            for l in t.labels() {
                if let Some(p) = clts.label_payload(*l) {
                    fired.extend(p.iter().cloned());
                }
            }
            let to = t.target();
            if !seen[to.index()] {
                seen[to.index()] = true;
                parent.insert(to, (s, labels_of(clts, t)));
                queue.push_back(to);
            }
        }
    }

    let path_to = |mut s: StateId<S>| -> SanityPath {
        let mut states = vec![name(clts, s)];
        let mut labels = Vec::new();
        while let Some((p, l)) = parent.get(&s) {
            labels.push(l.clone());
            states.push(name(clts, *p));
            s = *p;
        }
        states.reverse();
        labels.reverse();
        SanityPath { states, labels }
    };

    let deadlocks: Vec<SanityPath> = reachable
        .iter()
        .filter(|s| clts.outgoing(**s).is_empty())
        .map(|s| path_to(*s))
        .collect();

    let mut unreachable_states: Vec<String> = clts
        .states()
        .filter(|s| !seen[s.index()])
        .map(|s| name(clts, s))
        .collect();
    // A composition's member states that no reachable product state carries.
    if !members.is_empty() {
        let mut carried: Vec<HashSet<String>> = vec![HashSet::new(); members.len()];
        for s in &reachable {
            let parts: Vec<&str> = clts.state_name(*s).unwrap_or("").split('|').collect();
            if parts.len() == members.len() {
                for (i, p) in parts.iter().enumerate() {
                    carried[i].insert(p.to_string());
                }
            }
        }
        for (i, (mname, mclts)) in members.iter().enumerate() {
            for s in mclts.states() {
                let sn = name(mclts, s);
                if !carried[i].is_empty() && !carried[i].contains(&sn) {
                    unreachable_states.push(format!("{mname}.{sn}"));
                }
            }
        }
    }
    unreachable_states.sort();

    let labels_never_fired: Vec<String> = clts
        .alphabet()
        .into_iter()
        .chain(declared_labels.iter().cloned())
        .filter(|l| !fired.contains(l))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    // Tarjan SCCs over reachable states, following only internal-only transitions.
    let internal_only = |t: &crate::clts::Transition<S, L>| {
        !t.labels().is_empty() && t.labels().iter().all(|l| clts.is_internal_label(*l))
    };
    let mut index: HashMap<StateId<S>, usize> = HashMap::new();
    let mut low: HashMap<StateId<S>, usize> = HashMap::new();
    let mut on_stack: HashSet<StateId<S>> = HashSet::new();
    let mut stack: Vec<StateId<S>> = Vec::new();
    let mut next_index = 0usize;
    let mut internal_livelocks: Vec<Vec<String>> = Vec::new();
    for root in &reachable {
        if index.contains_key(root) {
            continue;
        }
        // Iterative Tarjan: (state, next edge position).
        let mut call: Vec<(StateId<S>, usize)> = vec![(*root, 0)];
        index.insert(*root, next_index);
        low.insert(*root, next_index);
        next_index += 1;
        stack.push(*root);
        on_stack.insert(*root);
        while let Some((v, pos)) = call.pop() {
            let edges: Vec<StateId<S>> = clts
                .outgoing(v)
                .iter()
                .filter(|t| internal_only(t))
                .map(|t| t.target())
                .collect();
            if pos < edges.len() {
                call.push((v, pos + 1));
                let w = edges[pos];
                match index.entry(w) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(next_index);
                        low.insert(w, next_index);
                        next_index += 1;
                        stack.push(w);
                        on_stack.insert(w);
                        call.push((w, 0));
                    }
                    std::collections::hash_map::Entry::Occupied(e) if on_stack.contains(&w) => {
                        let lw = *e.get();
                        let lv = low[&v];
                        low.insert(v, lv.min(lw));
                    }
                    std::collections::hash_map::Entry::Occupied(_) => {}
                }
                continue;
            }
            // v is finished: propagate to its caller, pop an SCC when v is its root.
            if let Some(&(u, _)) = call.last() {
                let lu = low[&u];
                let lv = low[&v];
                low.insert(u, lu.min(lv));
            }
            if low[&v] == index[&v] {
                let mut scc = Vec::new();
                while let Some(w) = stack.pop() {
                    on_stack.remove(&w);
                    scc.push(w);
                    if w == v {
                        break;
                    }
                }
                let cyclic = scc.len() > 1
                    || clts
                        .outgoing(v)
                        .iter()
                        .any(|t| internal_only(t) && t.target() == v);
                if cyclic {
                    let mut names: Vec<String> = scc.iter().map(|s| name(clts, *s)).collect();
                    names.sort();
                    internal_livelocks.push(names);
                }
            }
        }
    }
    internal_livelocks.sort();

    SanityReport {
        automaton: automaton.to_string(),
        reachable_states: reachable.len(),
        deadlocks,
        unreachable_states,
        labels_never_fired,
        internal_livelocks,
    }
}

impl std::fmt::Display for SanityReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "Sanity of '{}' ({} reachable states):",
            self.automaton, self.reachable_states
        )?;
        if self.is_clean() {
            return writeln!(
                f,
                "  clean: no deadlock, unreachable state, dead label or internal livelock"
            );
        }
        for d in &self.deadlocks {
            let mut path = String::new();
            for (i, s) in d.states.iter().enumerate() {
                if i > 0 {
                    path.push_str(&format!(" -{}-> ", d.labels[i - 1]));
                }
                path.push_str(s);
            }
            writeln!(
                f,
                "  deadlock: {}  (path: {path})",
                d.states.last().unwrap()
            )?;
        }
        for s in &self.unreachable_states {
            writeln!(f, "  unreachable state: {s}")?;
        }
        for l in &self.labels_never_fired {
            writeln!(f, "  label never fires: {l}")?;
        }
        for c in &self.internal_livelocks {
            writeln!(f, "  internal livelock: {{{}}}", c.join(", "))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn realize(src: &str) -> crate::context_dsl::realize::RealizedContext {
        let doc = crate::context_dsl::parse(src).expect("parses");
        crate::context_dsl::realize::realize(&doc, &[]).expect("realizes")
    }

    /// A single automaton: a terminal state is a deadlock with its path, an orphan state is
    /// unreachable, an alphabet label nothing takes never fires, and an internal-only cycle is a
    /// livelock.
    #[test]
    fn the_four_findings_on_one_automaton() {
        let r = realize(
            r#"
context s {
    alphabet { label go; label stop; label spin; label unused; }
    automata {
        automaton A {
            alphabet { label go; label stop; label spin; label unused; }
            internal { label spin; }
            states { state Init initial; state Busy; state Done; state Orphan; }
            transitions {
                transition Init -> Busy on label go;
                transition Busy -> Busy on label spin;
                transition Busy -> Done on label stop;
                transition Orphan -> Init on label go;
            }
        }
    }
}
"#,
        );
        let clts = r.context.clts("A").unwrap();
        let declared: Vec<String> = ["go", "stop", "spin", "unused"].map(String::from).to_vec();
        let rep = check("A", clts, &declared, &[]);
        assert_eq!(rep.reachable_states, 3);
        assert_eq!(rep.deadlocks.len(), 1);
        assert_eq!(rep.deadlocks[0].states, vec!["Init", "Busy", "Done"]);
        assert_eq!(rep.deadlocks[0].labels, vec!["go", "stop"]);
        assert_eq!(rep.unreachable_states, vec!["Orphan"]);
        assert_eq!(rep.labels_never_fired, vec!["unused"]);
        assert_eq!(rep.internal_livelocks, vec![vec!["Busy".to_string()]]);
        assert!(!rep.is_clean());
    }

    /// A clean model reports clean.
    #[test]
    fn a_clean_cycle_is_clean() {
        let r = realize(
            r#"
context c {
    alphabet { label a; label b; }
    automata {
        automaton A {
            states { state X initial; state Y; }
            transitions { transition X -> Y on label a; transition Y -> X on label b; }
        }
    }
}
"#,
        );
        let rep = check("A", r.context.clts("A").unwrap(), &[], &[]);
        assert!(rep.is_clean(), "{rep}");
    }
}
