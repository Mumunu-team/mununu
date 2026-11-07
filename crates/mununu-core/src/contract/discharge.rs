//! SCC-based discharge check for assume/guarantee contracts.
//!
//! Task A2 of `docs/archive/design/black-box-modules.md`. Given a `ContractSet`,
//! builds the directed graph where each clause is a node and each
//! `(discharger, dischargee)` edge means "this guarantee discharges that
//! assumption," then runs Tarjan SCC to detect cycles. The verdict tells
//! the user whether the standard non-circular Pnueli (1985) rule suffices,
//! whether circular reasoning is required (McMillan 1999), or whether the
//! graph is incomplete and the analysis must be conservative.
//!
//! The lightweight mu-rank McMillan-style check from task A8 (see §8.8 of
//! the design doc) is a future extension on top of this module — it will
//! attempt to discharge non-trivial SCCs before falling back to HITL.

use super::{ContractSet, DischargeEdge};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Outcome of running the discharge check on a `ContractSet`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DischargeVerdict {
    /// Every SCC is a singleton: the discharge order is a linear topological
    /// sort. The Pnueli 1985 rule applies — non-circular A/G suffices.
    Acyclic {
        /// One ordering of clauses that respects discharge dependencies.
        topological: Vec<String>,
        /// Assumptions that have no in-graph discharger but are declared as
        /// top-level environment assumptions (consistent with the user's
        /// declaration in `ContractSet::environment_assumptions`).
        unmet_environment: Vec<String>,
    },
    /// At least one non-trivial SCC, but every cycle is discharged by a
    /// mu-calculus rank witness (task A8 — lightweight McMillan-style
    /// inductive check). Mununu accepts these automatically with the
    /// provenance tag `mununu-verified circular discharge (mu-rank)`.
    /// The user is informed but not blocked.
    CircularWithRankWitness {
        /// Each cycle plus the rank witness that discharges it.
        cycles: Vec<RankWitnessedCycle>,
        /// Singletons in topological order.
        acyclic_remainder: Vec<String>,
    },
    /// At least one non-trivial SCC and at least one cycle has *no* rank
    /// witness (either some clause is missing `mu_rank`, or the rank
    /// pattern does not satisfy the lightweight McMillan rule). Mununu
    /// refuses to silently accept; HITL must approve.
    Circular {
        /// Every non-trivial SCC (each as the list of clause ids forming
        /// the cycle).
        cycles: Vec<Vec<String>>,
        /// Singletons (clauses outside any cycle) in topological order.
        acyclic_remainder: Vec<String>,
    },
    /// One or more clauses referenced in `discharges` or
    /// `environment_assumptions` are not present in the clause list. The
    /// graph is incomplete; treat every unresolved id as potentially in a
    /// cycle until it can be loaded.
    PotentiallyCircular {
        /// The ids that did not resolve.
        unresolved: Vec<String>,
        /// Best-effort verdict over the resolved portion.
        partial: Box<DischargeVerdict>,
    },
    /// Some assumptions in the set have neither an in-graph discharger nor
    /// an entry in `environment_assumptions`. Treated as an unmet obligation
    /// — distinct from a circular cycle because it is structurally fixable.
    Unmet {
        /// Assumption ids without any guarantor or top-level environment
        /// declaration.
        missing_dischargers: Vec<String>,
        /// Best-effort verdict over the rest.
        partial: Box<DischargeVerdict>,
    },
}

/// A cycle (non-trivial SCC) plus the rank witness that discharges it
/// via the lightweight McMillan-style check (task A8).
///
/// A witness exists when:
/// 1. Every clause in the cycle has `mu_rank` set.
/// 2. Walking the cycle in discharge-edge order yields rank deltas that
///    are strictly negative on every edge **except exactly one** base
///    edge (which may be non-negative). This corresponds to a
///    well-founded induction with a single inductive-step boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankWitnessedCycle {
    /// Clauses forming the cycle, in discharge-edge order.
    pub cycle: Vec<String>,
    /// The base edge — the (discharger, dischargee) pair where the rank
    /// "wraps" from the lowest back to a higher value. McMillan's
    /// inductive base.
    pub base_edge: (String, String),
}

impl DischargeVerdict {
    /// Whether mununu is willing to proceed to verification under this
    /// verdict without explicit user override.
    ///
    /// - `Acyclic` → always green to proceed (subject to unmet env list
    ///   being acceptable; that is a separate user decision).
    /// - `CircularWithRankWitness` → green: the cycle is discharged
    ///   automatically via the lightweight McMillan check (A8).
    /// - `Circular` → blocks; require HITL.
    /// - `PotentiallyCircular` → blocks; the user needs to refresh the
    ///   corpus or fill the gap.
    /// - `Unmet` → blocks; structural fix required.
    pub fn auto_proceed(&self) -> bool {
        matches!(
            self,
            DischargeVerdict::Acyclic { .. } | DischargeVerdict::CircularWithRankWitness { .. }
        )
    }
}

/// Run the discharge check on a contract set.
pub fn validate(set: &ContractSet) -> DischargeVerdict {
    // 1. Resolve unknown ids first. If any edge or env assumption refers
    //    to a missing clause, return PotentiallyCircular with the partial
    //    over the resolved portion.
    let unresolved = set.unknown_ids();
    if !unresolved.is_empty() {
        let resolved_set = strip_unresolved(set, &unresolved);
        let partial = validate(&resolved_set);
        return DischargeVerdict::PotentiallyCircular {
            unresolved,
            partial: Box::new(partial),
        };
    }

    // 2. Build the index: clause id → node index.
    let id_to_idx: HashMap<&str, usize> = set
        .clauses
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.as_str(), i))
        .collect();
    let n = set.clauses.len();

    // 3. Build adjacency list. Edge from discharger to dischargee — we use
    //    that direction so the SCC reflects "guarantor feeds consumer."
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for edge in &set.discharges {
        let (Some(&u), Some(&v)) = (
            id_to_idx.get(edge.discharger.as_str()),
            id_to_idx.get(edge.dischargee.as_str()),
        ) else {
            // Already excluded by unresolved check above; defensive.
            continue;
        };
        adj[u].push(v);
    }

    // 4. Tarjan SCC.
    let sccs = tarjan(&adj);

    // 5. Classify each SCC and collect cycles vs singletons.
    let mut cycles: Vec<Vec<String>> = Vec::new();
    let mut singletons: Vec<String> = Vec::new();
    for scc in &sccs {
        if scc.len() == 1 {
            // Singleton — but check for self-loop (still a cycle).
            let v = scc[0];
            if adj[v].contains(&v) {
                cycles.push(vec![set.clauses[v].id.clone()]);
            } else {
                singletons.push(set.clauses[v].id.clone());
            }
        } else {
            // Non-trivial SCC — by definition all nodes mutually reach.
            cycles.push(scc.iter().map(|&i| set.clauses[i].id.clone()).collect());
        }
    }

    // 6. Detect unmet assumptions: every Assumption clause must be either
    //    discharged by some edge or listed in environment_assumptions.
    let dischargee_ids: std::collections::HashSet<&str> = set
        .discharges
        .iter()
        .map(|e| e.dischargee.as_str())
        .collect();
    let env_ids: std::collections::HashSet<&str> = set
        .environment_assumptions
        .iter()
        .map(|s| s.as_str())
        .collect();
    let mut missing_dischargers: Vec<String> = set
        .clauses
        .iter()
        .filter(|c| c.kind.requires_discharge())
        .filter(|c| !dischargee_ids.contains(c.id.as_str()))
        .filter(|c| !env_ids.contains(c.id.as_str()))
        .map(|c| c.id.clone())
        .collect();
    missing_dischargers.sort();

    // 7. Try the lightweight McMillan-style discharge for any non-trivial
    //    SCCs. If every cycle has a rank witness, surface as
    //    `CircularWithRankWitness`; otherwise fall back to `Circular`.
    let witnessed_cycles: Option<Vec<RankWitnessedCycle>> = if cycles.is_empty() {
        None
    } else {
        try_mu_rank_witnesses(&cycles, set, &adj, &id_to_idx)
    };

    // 8. Build the underlying verdict from cycles vs acyclic remainder.
    let core = if cycles.is_empty() {
        // Walk SCC list in *reverse* order — Tarjan emits in reverse
        // topological order.
        let topological: Vec<String> = sccs
            .iter()
            .rev()
            .flat_map(|scc| scc.iter().map(|&i| set.clauses[i].id.clone()))
            .collect();
        let unmet_environment = set
            .environment_assumptions
            .iter()
            .filter(|id| id_to_idx.contains_key(id.as_str()))
            .filter(|id| !dischargee_ids.contains(id.as_str()))
            .cloned()
            .collect();
        DischargeVerdict::Acyclic {
            topological,
            unmet_environment,
        }
    } else if let Some(witnessed) = witnessed_cycles {
        let mut acyclic_remainder = singletons;
        acyclic_remainder.sort();
        DischargeVerdict::CircularWithRankWitness {
            cycles: witnessed,
            acyclic_remainder,
        }
    } else {
        let mut acyclic_remainder = singletons;
        acyclic_remainder.sort();
        DischargeVerdict::Circular {
            cycles,
            acyclic_remainder,
        }
    };

    if missing_dischargers.is_empty() {
        core
    } else {
        DischargeVerdict::Unmet {
            missing_dischargers,
            partial: Box::new(core),
        }
    }
}

/// Build a copy of the set with edges/env-assumptions that reference
/// unresolved ids stripped out, so we can compute a partial verdict over
/// the resolved portion.
fn strip_unresolved(set: &ContractSet, unresolved: &[String]) -> ContractSet {
    let unresolved_set: std::collections::HashSet<&str> =
        unresolved.iter().map(|s| s.as_str()).collect();
    let discharges: Vec<DischargeEdge> = set
        .discharges
        .iter()
        .filter(|e| {
            !unresolved_set.contains(e.discharger.as_str())
                && !unresolved_set.contains(e.dischargee.as_str())
        })
        .cloned()
        .collect();
    let environment_assumptions: Vec<String> = set
        .environment_assumptions
        .iter()
        .filter(|s| !unresolved_set.contains(s.as_str()))
        .cloned()
        .collect();
    ContractSet {
        clauses: set.clauses.clone(),
        discharges,
        environment_assumptions,
    }
}

/// Tarjan strongly-connected-components. Returns SCCs in reverse
/// topological order (the first SCC has no in-edges from later SCCs).
fn tarjan(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = adj.len();
    let mut index_counter: usize = 0;
    let mut stack: Vec<usize> = Vec::new();
    let mut on_stack: Vec<bool> = vec![false; n];
    let mut indices: Vec<Option<usize>> = vec![None; n];
    let mut lowlink: Vec<usize> = vec![0; n];
    let mut result: Vec<Vec<usize>> = Vec::new();

    // Iterative Tarjan to avoid recursion-depth issues on pathological
    // contract sets.
    for start in 0..n {
        if indices[start].is_some() {
            continue;
        }
        // Each frame on `call_stack` is (node, iter_position).
        let mut call_stack: Vec<(usize, usize)> = Vec::new();
        indices[start] = Some(index_counter);
        lowlink[start] = index_counter;
        index_counter += 1;
        stack.push(start);
        on_stack[start] = true;
        call_stack.push((start, 0));

        while let Some(&(v, i)) = call_stack.last() {
            if i < adj[v].len() {
                let w = adj[v][i];
                // Advance the iterator at this frame before descending.
                call_stack.last_mut().unwrap().1 += 1;
                if indices[w].is_none() {
                    indices[w] = Some(index_counter);
                    lowlink[w] = index_counter;
                    index_counter += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    call_stack.push((w, 0));
                } else if on_stack[w] {
                    let w_idx = indices[w].unwrap();
                    if w_idx < lowlink[v] {
                        lowlink[v] = w_idx;
                    }
                }
            } else {
                // Done with this node — pop and propagate lowlink.
                call_stack.pop();
                let v_idx = indices[v].unwrap();
                if lowlink[v] == v_idx {
                    let mut scc = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        scc.push(w);
                        if w == v {
                            break;
                        }
                    }
                    scc.reverse();
                    result.push(scc);
                }
                if let Some(&(parent, _)) = call_stack.last()
                    && lowlink[v] < lowlink[parent]
                {
                    lowlink[parent] = lowlink[v];
                }
            }
        }
    }

    result
}

/// Attempt to discharge every non-trivial SCC via the lightweight
/// McMillan-style rank check (task A8). Returns `Some(witnesses)` iff
/// every cycle has a rank witness; returns `None` if any cycle lacks
/// one (the caller falls back to the full `Circular` verdict).
///
/// **The rule.** For each cycle (in discharge-edge order):
/// 1. Every clause must have `mu_rank` set.
/// 2. Walking the cycle as a closed loop, count edges with non-negative
///    rank delta (`rank(dischargee) - rank(discharger) >= 0`). Exactly
///    one such edge is allowed — the inductive **base edge**. All other
///    edges must have strictly negative delta (rank strictly decreasing
///    along the cycle).
/// 3. If the rule holds, emit a `RankWitnessedCycle` naming the base
///    edge.
///
/// This is intentionally conservative — it catches well-formed cycles
/// like arbiter↔master fairness pairs without trying to verify a full
/// step-indexed McMillan derivation. Cycles that *might* be sound under
/// a more powerful check fall back to HITL, which is the right safety
/// posture per Document A §7 Q2.
fn try_mu_rank_witnesses(
    cycles: &[Vec<String>],
    set: &ContractSet,
    adj: &[Vec<usize>],
    id_to_idx: &HashMap<&str, usize>,
) -> Option<Vec<RankWitnessedCycle>> {
    let mut witnesses = Vec::with_capacity(cycles.len());
    for cycle in cycles {
        let witness = mu_rank_witness_for_cycle(cycle, set, adj, id_to_idx)?;
        witnesses.push(witness);
    }
    Some(witnesses)
}

fn mu_rank_witness_for_cycle(
    cycle: &[String],
    set: &ContractSet,
    adj: &[Vec<usize>],
    id_to_idx: &HashMap<&str, usize>,
) -> Option<RankWitnessedCycle> {
    // Collect indices in cycle.
    let cycle_indices: Vec<usize> = cycle
        .iter()
        .filter_map(|id| id_to_idx.get(id.as_str()).copied())
        .collect();
    if cycle_indices.len() != cycle.len() {
        return None;
    }

    // Every clause must have a mu_rank.
    let ranks: Vec<u32> = cycle_indices
        .iter()
        .map(|&i| set.clauses[i].mu_rank)
        .collect::<Option<Vec<_>>>()?;

    // Special case: singleton self-loop. A self-loop is always a base
    // edge; no decreasing edges to count. Discharge iff rank exists.
    if cycle.len() == 1 {
        let id = &cycle[0];
        return Some(RankWitnessedCycle {
            cycle: vec![id.clone()],
            base_edge: (id.clone(), id.clone()),
        });
    }

    // Walk the cycle in discharge-edge order. We need to determine that
    // order from `adj`: starting at cycle[0], follow edges that lead to
    // other members of the cycle.
    let in_cycle: std::collections::HashSet<usize> = cycle_indices.iter().copied().collect();
    let mut ordered_indices = Vec::with_capacity(cycle.len());
    let mut visited: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut current = cycle_indices[0];
    ordered_indices.push(current);
    visited.insert(current);

    while ordered_indices.len() < cycle.len() {
        // Find a successor of `current` that is still in the cycle and
        // unvisited.
        let next = adj[current]
            .iter()
            .find(|&&w| in_cycle.contains(&w) && !visited.contains(&w))?;
        ordered_indices.push(*next);
        visited.insert(*next);
        current = *next;
    }

    // The cycle closes back to ordered_indices[0]. Verify there is an
    // edge from the last node back to the first; otherwise this isn't
    // a true cycle in this orientation.
    let last = *ordered_indices.last().unwrap();
    if !adj[last].contains(&ordered_indices[0]) {
        return None;
    }

    // Count rank deltas along each edge of the ordered cycle, including
    // the closing edge.
    let rank_by_idx: HashMap<usize, u32> = cycle_indices
        .iter()
        .zip(ranks.iter())
        .map(|(&i, &r)| (i, r))
        .collect();

    let mut base_edge: Option<(String, String)> = None;
    let n = ordered_indices.len();
    for step in 0..n {
        let from = ordered_indices[step];
        let to = ordered_indices[(step + 1) % n];
        let delta = i64::from(rank_by_idx[&to]) - i64::from(rank_by_idx[&from]);
        if delta < 0 {
            continue; // strictly descending — good
        }
        // Non-negative edge — must be the (unique) base edge.
        if base_edge.is_some() {
            return None; // more than one non-descending edge
        }
        base_edge = Some((set.clauses[from].id.clone(), set.clauses[to].id.clone()));
    }

    // A well-formed cycle has exactly one base edge.
    let base_edge = base_edge?;
    let ordered_ids: Vec<String> = ordered_indices
        .iter()
        .map(|&i| set.clauses[i].id.clone())
        .collect();
    Some(RankWitnessedCycle {
        cycle: ordered_ids,
        base_edge,
    })
}

#[cfg(test)]
mod tests {
    use super::super::{ClauseKind, ClauseProvenance, ContractClause};
    use super::*;

    fn clause(id: &str, kind: ClauseKind, owner: &str) -> ContractClause {
        ContractClause {
            id: id.to_string(),
            kind,
            owner: owner.to_string(),
            description: None,
            provenance: ClauseProvenance::UserAuthored,
            mu_rank: None,
        }
    }

    fn edge(discharger: &str, dischargee: &str) -> DischargeEdge {
        DischargeEdge {
            discharger: discharger.to_string(),
            dischargee: dischargee.to_string(),
        }
    }

    #[test]
    fn acyclic_chain_reports_topological_order() {
        // G_a discharges A_b; G_b discharges A_c. Linear chain.
        let set = ContractSet {
            clauses: vec![
                clause("G_a", ClauseKind::Guarantee, "a"),
                clause("A_b", ClauseKind::Assumption, "b"),
                clause("G_b", ClauseKind::Guarantee, "b"),
                clause("A_c", ClauseKind::Assumption, "c"),
            ],
            discharges: vec![edge("G_a", "A_b"), edge("G_b", "A_c")],
            environment_assumptions: vec![],
        };
        match validate(&set) {
            DischargeVerdict::Acyclic { topological, .. } => {
                assert_eq!(topological.len(), 4);
                let pos = |id: &str| topological.iter().position(|s| s == id).unwrap();
                assert!(pos("G_a") < pos("A_b"));
                assert!(pos("G_b") < pos("A_c"));
            }
            other => panic!("expected Acyclic, got {:?}", other),
        }
    }

    #[test]
    fn self_loop_is_a_cycle() {
        let set = ContractSet {
            clauses: vec![clause("X", ClauseKind::Guarantee, "x")],
            discharges: vec![edge("X", "X")],
            environment_assumptions: vec![],
        };
        match validate(&set) {
            DischargeVerdict::Circular { cycles, .. } => {
                assert_eq!(cycles, vec![vec!["X".to_string()]]);
            }
            other => panic!("expected Circular, got {:?}", other),
        }
    }

    #[test]
    fn two_node_cycle_is_circular() {
        // A's guarantee discharges B's assumption; B's guarantee discharges A's.
        let set = ContractSet {
            clauses: vec![
                clause("G_a", ClauseKind::Guarantee, "a"),
                clause("A_a", ClauseKind::Assumption, "a"),
                clause("G_b", ClauseKind::Guarantee, "b"),
                clause("A_b", ClauseKind::Assumption, "b"),
            ],
            discharges: vec![
                edge("G_a", "A_b"),
                edge("G_b", "A_a"),
                edge("A_b", "G_b"),
                edge("A_a", "G_a"),
            ],
            environment_assumptions: vec![],
        };
        match validate(&set) {
            DischargeVerdict::Circular { cycles, .. } => {
                assert_eq!(cycles.len(), 1);
                let cycle = &cycles[0];
                assert_eq!(cycle.len(), 4);
                for expected in ["G_a", "A_a", "G_b", "A_b"] {
                    assert!(
                        cycle.iter().any(|id| id == expected),
                        "cycle should contain {}, got {:?}",
                        expected,
                        cycle
                    );
                }
            }
            other => panic!("expected Circular, got {:?}", other),
        }
    }

    #[test]
    fn unresolved_id_yields_potentially_circular() {
        let set = ContractSet {
            clauses: vec![clause("A_x", ClauseKind::Assumption, "x")],
            // Discharger does not exist in clauses.
            discharges: vec![edge("G_missing", "A_x")],
            environment_assumptions: vec![],
        };
        match validate(&set) {
            DischargeVerdict::PotentiallyCircular { unresolved, .. } => {
                assert_eq!(unresolved, vec!["G_missing".to_string()]);
            }
            other => panic!("expected PotentiallyCircular, got {:?}", other),
        }
    }

    #[test]
    fn unmet_assumption_without_env_is_flagged() {
        // A_b has no discharger and is not declared as env.
        let set = ContractSet {
            clauses: vec![
                clause("G_a", ClauseKind::Guarantee, "a"),
                clause("A_b", ClauseKind::Assumption, "b"),
            ],
            discharges: vec![],
            environment_assumptions: vec![],
        };
        match validate(&set) {
            DischargeVerdict::Unmet {
                missing_dischargers,
                partial,
            } => {
                assert_eq!(missing_dischargers, vec!["A_b".to_string()]);
                // Partial verdict over the rest should be Acyclic
                // (since the graph has no edges).
                assert!(matches!(*partial, DischargeVerdict::Acyclic { .. }));
            }
            other => panic!("expected Unmet, got {:?}", other),
        }
    }

    #[test]
    fn env_assumption_satisfies_obligation() {
        let set = ContractSet {
            clauses: vec![
                clause("G_a", ClauseKind::Guarantee, "a"),
                clause("A_b", ClauseKind::Assumption, "b"),
            ],
            discharges: vec![],
            environment_assumptions: vec!["A_b".to_string()],
        };
        match validate(&set) {
            DischargeVerdict::Acyclic { .. } => {}
            other => panic!("expected Acyclic, got {:?}", other),
        }
    }

    #[test]
    fn invariant_can_discharge_assumption() {
        let set = ContractSet {
            clauses: vec![
                clause("Inv_clock", ClauseKind::Invariant, "clock"),
                clause("A_module", ClauseKind::Assumption, "module"),
            ],
            discharges: vec![edge("Inv_clock", "A_module")],
            environment_assumptions: vec![],
        };
        match validate(&set) {
            DischargeVerdict::Acyclic { topological, .. } => {
                assert_eq!(topological.len(), 2);
                let pos = |id: &str| topological.iter().position(|s| s == id).unwrap();
                assert!(pos("Inv_clock") < pos("A_module"));
            }
            other => panic!("expected Acyclic, got {:?}", other),
        }
    }

    fn ranked_clause(id: &str, kind: ClauseKind, owner: &str, rank: u32) -> ContractClause {
        ContractClause {
            id: id.to_string(),
            kind,
            owner: owner.to_string(),
            description: None,
            provenance: ClauseProvenance::UserAuthored,
            mu_rank: Some(rank),
        }
    }

    #[test]
    fn rank_witness_discharges_two_node_cycle_with_ranks() {
        // Same cycle shape as `two_node_cycle_is_circular` but every
        // clause carries a mu_rank. The lightweight check should accept.
        // Rank ordering (clockwise around the cycle G_a → A_b → G_b → A_a → G_a):
        // ranks 4 → 3 → 2 → 1 → (back to 4 — the base edge).
        let set = ContractSet {
            clauses: vec![
                ranked_clause("G_a", ClauseKind::Guarantee, "a", 4),
                ranked_clause("A_b", ClauseKind::Assumption, "b", 3),
                ranked_clause("G_b", ClauseKind::Guarantee, "b", 2),
                ranked_clause("A_a", ClauseKind::Assumption, "a", 1),
            ],
            discharges: vec![
                edge("G_a", "A_b"),
                edge("A_b", "G_b"),
                edge("G_b", "A_a"),
                edge("A_a", "G_a"),
            ],
            environment_assumptions: vec![],
        };
        match validate(&set) {
            DischargeVerdict::CircularWithRankWitness { cycles, .. } => {
                assert_eq!(cycles.len(), 1);
                let witness = &cycles[0];
                assert_eq!(witness.cycle.len(), 4);
                // Base edge should wrap from the lowest rank (1) back up.
                assert_eq!(witness.base_edge.0, "A_a");
                assert_eq!(witness.base_edge.1, "G_a");
            }
            other => panic!("expected CircularWithRankWitness, got {:?}", other),
        }
    }

    #[test]
    fn rank_witness_rejects_cycle_with_two_non_descending_edges() {
        // Ranks 4 → 5 → 6 → 1 → 4 has *two* non-descending edges
        // (4→5 and 5→6), so the lightweight check must reject.
        let set = ContractSet {
            clauses: vec![
                ranked_clause("G_a", ClauseKind::Guarantee, "a", 4),
                ranked_clause("A_b", ClauseKind::Assumption, "b", 5),
                ranked_clause("G_b", ClauseKind::Guarantee, "b", 6),
                ranked_clause("A_a", ClauseKind::Assumption, "a", 1),
            ],
            discharges: vec![
                edge("G_a", "A_b"),
                edge("A_b", "G_b"),
                edge("G_b", "A_a"),
                edge("A_a", "G_a"),
            ],
            environment_assumptions: vec![],
        };
        match validate(&set) {
            DischargeVerdict::Circular { cycles, .. } => {
                assert_eq!(cycles.len(), 1);
            }
            other => panic!("expected Circular (no witness), got {:?}", other),
        }
    }

    #[test]
    fn rank_witness_rejects_cycle_with_missing_rank() {
        // One clause has no mu_rank → witness check fails, fall back.
        let set = ContractSet {
            clauses: vec![
                ranked_clause("G_a", ClauseKind::Guarantee, "a", 2),
                ranked_clause("A_b", ClauseKind::Assumption, "b", 1),
                clause("G_b", ClauseKind::Guarantee, "b"), // no rank
                ranked_clause("A_a", ClauseKind::Assumption, "a", 0),
            ],
            discharges: vec![
                edge("G_a", "A_b"),
                edge("A_b", "G_b"),
                edge("G_b", "A_a"),
                edge("A_a", "G_a"),
            ],
            environment_assumptions: vec![],
        };
        match validate(&set) {
            DischargeVerdict::Circular { .. } => {}
            other => panic!("expected Circular (rank missing), got {:?}", other),
        }
    }

    #[test]
    fn self_loop_with_rank_is_witnessed() {
        let set = ContractSet {
            clauses: vec![ranked_clause("X", ClauseKind::Guarantee, "x", 0)],
            discharges: vec![edge("X", "X")],
            environment_assumptions: vec![],
        };
        match validate(&set) {
            DischargeVerdict::CircularWithRankWitness { cycles, .. } => {
                assert_eq!(cycles.len(), 1);
                assert_eq!(cycles[0].cycle, vec!["X".to_string()]);
                assert_eq!(cycles[0].base_edge, ("X".to_string(), "X".to_string()));
            }
            other => panic!("expected witnessed self-loop, got {:?}", other),
        }
    }

    #[test]
    fn self_loop_without_rank_falls_back_to_circular() {
        let set = ContractSet {
            clauses: vec![clause("X", ClauseKind::Guarantee, "x")],
            discharges: vec![edge("X", "X")],
            environment_assumptions: vec![],
        };
        match validate(&set) {
            DischargeVerdict::Circular { cycles, .. } => {
                assert_eq!(cycles, vec![vec!["X".to_string()]]);
            }
            other => panic!("expected Circular, got {:?}", other),
        }
    }

    #[test]
    fn auto_proceed_extends_to_witnessed_circular() {
        assert!(
            DischargeVerdict::CircularWithRankWitness {
                cycles: vec![],
                acyclic_remainder: vec![],
            }
            .auto_proceed()
        );
    }

    #[test]
    fn auto_proceed_only_on_acyclic() {
        assert!(
            DischargeVerdict::Acyclic {
                topological: vec![],
                unmet_environment: vec![]
            }
            .auto_proceed()
        );
        assert!(
            !DischargeVerdict::Circular {
                cycles: vec![],
                acyclic_remainder: vec![]
            }
            .auto_proceed()
        );
        assert!(
            !DischargeVerdict::Unmet {
                missing_dischargers: vec![],
                partial: Box::new(DischargeVerdict::Acyclic {
                    topological: vec![],
                    unmet_environment: vec![]
                })
            }
            .auto_proceed()
        );
    }
}
