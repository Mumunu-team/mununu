//! Per-component contracts (assume-guarantee) and discharge graph machinery.
//!
//! See `docs/archive/design/black-box-modules.md` for the conceptual frame.
//!
//! This module is the minimal foundation for tasks A1 and A2 of that
//! document's implementation plan: a `Contract` value type plus a
//! `discharge` submodule that runs Tarjan SCC over the dependency graph
//! formed by `guarantor → consumer` edges.
//!
//! The full CTXDSL grammar extension for inline `contract { ... }` blocks
//! is deferred to a follow-up task; this module currently consumes contracts
//! as a JSON-serialisable Rust value.

pub mod contract_uri;
pub mod discharge;
pub mod discover;
pub mod gap;
pub mod review;

use serde::{Deserialize, Serialize};
use std::fmt;

/// A single clause in a contract — either an assumption the owning module
/// makes about its environment, or a guarantee the owning module provides.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContractClause {
    /// Stable identifier within the contract set. Used as the node id in
    /// the discharge graph.
    pub id: String,
    /// Which side of the contract this clause sits on.
    pub kind: ClauseKind,
    /// Name of the module/component that owns this clause.
    pub owner: String,
    /// Optional human-readable description; survives into reports.
    #[serde(default)]
    pub description: Option<String>,
    /// Provenance — where the clause came from.
    #[serde(default)]
    pub provenance: ClauseProvenance,
    /// Optional mu-calculus rank used by the lightweight McMillan-style
    /// circular-discharge check (task A8). Roughly: the alternation depth
    /// at which this clause's underlying fixpoint sits. Larger rank =
    /// further from the base case. Cycles whose ranks strictly decrease
    /// at every edge except one designated base edge can be discharged
    /// by well-founded induction (see `discharge::mu_rank_witness`).
    #[serde(default)]
    pub mu_rank: Option<u32>,
}

/// Kind of contract clause. Mirrors the IR's `PropertyRole` enum but
/// narrowed to the two A/G sides plus an explicit *invariant* slot
/// (always-true guarantee with universal temporal scope).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClauseKind {
    /// An environment assumption — the module relies on this holding.
    Assumption,
    /// A guarantee the module provides under its assumptions.
    Guarantee,
    /// A guarantee with universal temporal scope (mu-calculus `G φ` form).
    /// Treated like `Guarantee` in the discharge graph.
    Invariant,
}

impl ClauseKind {
    /// Whether this clause is something a sibling module can discharge as
    /// an assumption (i.e. it is a guarantee or invariant produced by some
    /// module).
    pub fn provides_dischargee(self) -> bool {
        matches!(self, ClauseKind::Guarantee | ClauseKind::Invariant)
    }

    /// Whether this clause requires discharge (i.e. it is an assumption
    /// that must be guaranteed by either a sibling or the top-level env).
    pub fn requires_discharge(self) -> bool {
        matches!(self, ClauseKind::Assumption)
    }
}

/// Where a clause came from. Used for audit trails and to flag the trust
/// boundary at HITL review time.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClauseProvenance {
    /// Hand-authored by the user.
    UserAuthored,
    /// Looked up from the contract corpus (Document D).
    Corpus { id: String },
    /// Discovered from source-comment annotations.
    SourceComment,
    /// Proposed by mununu (template-derived or L*-learned).
    MununuProposed,
    /// Accepted from a vendor's external datasheet.
    VendorContract,
    /// Provenance not declared.
    #[default]
    Unknown,
}

/// A directed edge in the discharge graph: `discharger` (a guarantee or
/// invariant) is claimed to discharge `dischargee` (an assumption).
///
/// Both fields are clause `id`s and must match clauses in the same
/// `ContractSet`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DischargeEdge {
    /// Id of the guarantee/invariant doing the discharging.
    pub discharger: String,
    /// Id of the assumption being discharged.
    pub dischargee: String,
}

/// A full contract set: the clauses across one or more modules and the
/// claimed discharges among them. This is what `discharge::validate`
/// operates on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractSet {
    /// All clauses across all modules in this set.
    pub clauses: Vec<ContractClause>,
    /// Claimed discharges. Each entry is a `(guarantor, consumer)` pair
    /// of clause ids.
    pub discharges: Vec<DischargeEdge>,
    /// Optional top-level environment assumptions — clauses whose
    /// dischargee is "the outside world" and require no in-graph
    /// guarantor.
    #[serde(default)]
    pub environment_assumptions: Vec<String>,
}

impl ContractSet {
    /// Look up a clause by id.
    pub fn clause(&self, id: &str) -> Option<&ContractClause> {
        self.clauses.iter().find(|c| c.id == id)
    }

    /// Whether every id mentioned in `discharges` and
    /// `environment_assumptions` corresponds to a known clause.
    /// Returns the list of unknown ids (empty when the set is well-formed).
    pub fn unknown_ids(&self) -> Vec<String> {
        let mut unknown = Vec::new();
        let known: std::collections::HashSet<&str> =
            self.clauses.iter().map(|c| c.id.as_str()).collect();
        for edge in &self.discharges {
            if !known.contains(edge.discharger.as_str()) {
                unknown.push(edge.discharger.clone());
            }
            if !known.contains(edge.dischargee.as_str()) {
                unknown.push(edge.dischargee.clone());
            }
        }
        for env_id in &self.environment_assumptions {
            if !known.contains(env_id.as_str()) {
                unknown.push(env_id.clone());
            }
        }
        unknown.sort();
        unknown.dedup();
        unknown
    }
}

impl fmt::Display for ClauseKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClauseKind::Assumption => write!(f, "assumption"),
            ClauseKind::Guarantee => write!(f, "guarantee"),
            ClauseKind::Invariant => write!(f, "invariant"),
        }
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn clause_kind_dischargee_classification() {
        assert!(ClauseKind::Guarantee.provides_dischargee());
        assert!(ClauseKind::Invariant.provides_dischargee());
        assert!(!ClauseKind::Assumption.provides_dischargee());
        assert!(ClauseKind::Assumption.requires_discharge());
    }

    #[test]
    fn contract_set_unknown_ids_detects_dangling_edges() {
        let set = ContractSet {
            clauses: vec![
                clause("G_master", ClauseKind::Guarantee, "master"),
                clause("A_arbiter", ClauseKind::Assumption, "arbiter"),
            ],
            discharges: vec![
                DischargeEdge {
                    discharger: "G_master".to_string(),
                    dischargee: "A_arbiter".to_string(),
                },
                DischargeEdge {
                    discharger: "G_missing".to_string(),
                    dischargee: "A_arbiter".to_string(),
                },
            ],
            environment_assumptions: vec!["A_top".to_string()],
        };

        let unknown = set.unknown_ids();
        assert_eq!(unknown, vec!["A_top".to_string(), "G_missing".to_string()]);
    }

    #[test]
    fn contract_set_well_formed_returns_empty_unknown() {
        let set = ContractSet {
            clauses: vec![
                clause("G_a", ClauseKind::Guarantee, "a"),
                clause("A_b", ClauseKind::Assumption, "b"),
            ],
            discharges: vec![DischargeEdge {
                discharger: "G_a".to_string(),
                dischargee: "A_b".to_string(),
            }],
            environment_assumptions: vec![],
        };
        assert!(set.unknown_ids().is_empty());
    }

    #[test]
    fn clause_kind_display() {
        assert_eq!(ClauseKind::Assumption.to_string(), "assumption");
        assert_eq!(ClauseKind::Guarantee.to_string(), "guarantee");
        assert_eq!(ClauseKind::Invariant.to_string(), "invariant");
    }

    #[test]
    fn contract_set_clause_lookup() {
        let set = ContractSet {
            clauses: vec![clause("A_x", ClauseKind::Assumption, "x")],
            discharges: vec![],
            environment_assumptions: vec![],
        };
        assert_eq!(set.clause("A_x").map(|c| c.id.as_str()), Some("A_x"));
        assert_eq!(set.clause("missing"), None);
    }
}
