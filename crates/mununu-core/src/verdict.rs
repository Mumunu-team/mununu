//! Canonical property-verdict vocabulary — the single source of truth for the
//! verdict string every verify surface reports.
//!
//! Model checking answers a property with one of four outcomes. Historically each
//! surface spelled them differently — `holds`/`violated`/`unknown` on the flagship
//! `sv verify-auto` and extraction, but `reachable`/`unreachable`/`contradiction` on
//! `btor2 verify` and `inconclusive` on `btor2 verify-liveness`. [`PropertyVerdict`]
//! fixes ONE spelling and every surface maps its engine-specific verdict into it via
//! the `From` impls here, so the surfaces cannot drift.
//!
//! The vocabulary is the one the flagship already uses: `holds` / `violated` /
//! `unknown` / `skipped`. Surface-specific *detail* (which engines decided, a
//! soundness-contradiction alarm, cube-cell counts) stays in the surface's own
//! response fields alongside this canonical verdict.

use crate::adapter::btor2::symbolic_bitblast::ExactVerdict;
use crate::adapter::liveness_rescue::LivenessVerdict;
use crate::adapter::reach_portfolio::ReachVerdict;

/// The canonical answer to "did the property hold?", shared by every verify surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub enum PropertyVerdict {
    /// The property holds on every reachable state / path — a sound proof.
    Holds,
    /// The property is violated — a real counterexample exists.
    Violated,
    /// No engine decided: abstention, over-cap, timeout, or a soundness alarm. The
    /// caller surfaces the *reason* (e.g. a `contradiction` flag) separately.
    Unknown,
    /// The property was not evaluated — out of the supported fragment, or filtered
    /// out before the run.
    Skipped,
}

/// A structured elaboration of a [`PropertyVerdict`], carried ALONGSIDE the canonical verdict —
/// never replacing it. `None`/empty on the common path. This is how a bare `⊥`/`violated` becomes an
/// *actionable* result: which configs hold, under what assumption it would hold, or that the target is
/// simply never reachable. It follows the same contract as the cube-cell / countertrace detail Track I
/// already carries: **a refinement is a diagnostic and NEVER changes the canonical verdict** — a
/// config-scoped or assumption-scoped result keeps the canonical verdict `Unknown` (the honest
/// unconditional answer; an assumption is not monotone for `AG EF`, so it cannot soundly transfer).
///
/// Phases (see `.claude/plans/refined-verdicts-assumption-discovery.md`): Phase 0 populates
/// [`Self::vacuous`] + [`Self::bot_diagnosis`]; Phase 1 populates [`Self::config_partition`]; Phase 2
/// populates [`Self::holds_under`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub struct VerdictRefinement {
    /// The recovery target is never reachable from the initial state — `AG EF(good)` is degenerate
    /// (the target is never entered), so the plain verdict is misleading. A SOUND witness (the
    /// reachability portfolio proved `good` unreachable; only emitted when that proof is sound —
    /// i.e. not on free-init state, per `ReachVerdict::Unreachable`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub vacuous: Option<VacuityWitness>,
    /// A config-scoped partition: the property depends on config values (Phase 1 / capability A).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub config_partition: Option<ConfigPartition>,
    /// Environment assumption(s) under which the property holds (Phase 2 / capability B). Each is a
    /// CONDITIONAL result (`HoldsUnder(φ)`) — the canonical verdict stays `Unknown`.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub holds_under: Vec<DiscoveredAssumption>,
    /// The best-effort structural "why ⊥ / what would decide it" hint (promoted from the standalone
    /// [`crate::adapter::recoverability::diagnose_recoverability_bot`]).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub bot_diagnosis: Option<crate::adapter::recoverability::RecoverabilityBotDiagnosis>,
}

impl VerdictRefinement {
    /// True when no refinement was produced — the bare canonical verdict stands.
    pub fn is_empty(&self) -> bool {
        self.vacuous.is_none()
            && self.config_partition.is_none()
            && self.holds_under.is_empty()
            && self.bot_diagnosis.is_none()
    }
}

/// See [`VerdictRefinement::vacuous`]. The recovery target is never reached.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub struct VacuityWitness {
    /// `good` is unreachable from the initial state (a sound reachability-portfolio `Unreachable`).
    pub good_unreachable: bool,
    /// A one-line, user-facing explanation.
    pub note: String,
}

/// A concrete assignment of config leaves to values — one point in the config space.
pub type ConfigValuation = Vec<(String, u64)>;

/// See [`VerdictRefinement::config_partition`]. The property's verdict partitioned over the config
/// leaves the recovery rides on. Sound per cell (each is a concrete pinned model decided by
/// exact-symbolic); `exhaustive` is true ONLY when the enumerated config set is the complete reachable
/// set. (Populated in Phase 1.)
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub struct ConfigPartition {
    /// The config leaves case-split on: `(name, width)`.
    pub config_atoms: Vec<(String, u32)>,
    /// Config valuations for which the property HOLDS.
    pub holds: Vec<ConfigValuation>,
    /// … for which it is VIOLATED.
    pub violated: Vec<ConfigValuation>,
    /// … for which it stayed ⊥ even pinned.
    pub unknown: Vec<ConfigValuation>,
    /// … for which the target is vacuous (never reached under that config).
    pub vacuous: Vec<ConfigValuation>,
    /// True iff the enumerated config set is the COMPLETE reachable config set (else the partition is
    /// over the enumerated/reachable subset only).
    pub exhaustive: bool,
    /// The deciding engine, for provenance (e.g. `"exact-symbolic per-config pin"`).
    pub engine: String,
}

/// The shape of a discovered environment assumption.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub enum AssumptionKind {
    /// An input held at a constant value (`en = 1`).
    InputHold,
    /// A CONJUNCTION of environment-input holds (`ack == 1 && escalate == 0`) — the minimal set of
    /// environment inputs that must be constrained for a two-player game to become realizable, when no
    /// single hold suffices (the design has multiple independent adversarial blockers).
    InputConjunction,
    /// A finite input schedule (a command sequence).
    InputSchedule,
    /// "Reset is eventually asserted."
    ResetEventually,
    /// A synthesized positional environment strategy (1-player: the environment owns all inputs).
    EnvStrategy,
    /// A LIVENESS / fairness environment assumption `GF(in == v)` — the environment input holds `v`
    /// INFINITELY OFTEN — under which an unrealizable RECURRENCE (Büchi) game `GF good` becomes realizable
    /// (`GF a → GF good`, the GR(1) 1-pair objective). Distinct from `InputHold` (a SAFETY hold `G(a)`):
    /// fairness is strictly weaker (the environment may violate `a` finitely often).
    InputFairness,
    /// A CONJUNCTION of fairness assumptions `GF(e_1 == v_1) ∧ … ∧ GF(e_k == v_k)` — the minimal set of
    /// environment liveness assumptions under which an unrealizable RECURRENCE game becomes realizable
    /// (`(⋀_i GF a_i) → GF good`, the MULTI-pair GR(1) objective), when no SINGLE fairness assumption
    /// suffices (multiple independent liveness blockers). The fairness analog of `InputConjunction`.
    InputFairnessConjunction,
}

/// See [`VerdictRefinement::holds_under`]. A CONDITIONAL result: the property holds under `phi`.
/// (Populated in Phase 2.)
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub struct DiscoveredAssumption {
    /// A human-readable predicate over named inputs (Phase 2b may carry a typed schedule/strategy).
    pub phi: String,
    /// The assumption's shape.
    pub kind: AssumptionKind,
    /// True iff `good` is genuinely reached under `phi` (the non-vacuity gate passed).
    pub non_vacuous: bool,
    /// The engine that discovered it, for provenance.
    pub engine: String,
    /// mununu#1175 — what the reader should know about the non-vacuity gate's own evidence: one
    /// line per reach-portfolio member whose path (to the target, or out of it) the concrete
    /// simulator did not confirm. Absent when every path was confirmed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl PropertyVerdict {
    /// The stable lowercase label — identical across CLI, HTTP API, and UI.
    pub fn as_str(self) -> &'static str {
        match self {
            PropertyVerdict::Holds => "holds",
            PropertyVerdict::Violated => "violated",
            PropertyVerdict::Unknown => "unknown",
            PropertyVerdict::Skipped => "skipped",
        }
    }
}

impl From<ReachVerdict> for PropertyVerdict {
    /// The safety reading of `bad`-reachability: `bad` **unreachable** = the
    /// assertion HOLDS; **reachable** = VIOLATED; undecided or a contradiction alarm
    /// = UNKNOWN (the alarm is surfaced separately by the caller's `contradiction`
    /// flag + the per-engine `reachable_by` / `unreachable_by` breakdown).
    fn from(v: ReachVerdict) -> Self {
        match v {
            ReachVerdict::Unreachable => PropertyVerdict::Holds,
            ReachVerdict::Reachable => PropertyVerdict::Violated,
            ReachVerdict::Unknown | ReachVerdict::Contradiction => PropertyVerdict::Unknown,
        }
    }
}

impl From<LivenessVerdict> for PropertyVerdict {
    fn from(v: LivenessVerdict) -> Self {
        match v {
            LivenessVerdict::Holds => PropertyVerdict::Holds,
            LivenessVerdict::Violated => PropertyVerdict::Violated,
            LivenessVerdict::Inconclusive => PropertyVerdict::Unknown,
        }
    }
}

impl From<ExactVerdict> for PropertyVerdict {
    /// The exact 3-valued engine gives a *definite* verdict within its cap (the
    /// over-cap / unsupported case is an `Err` the caller maps to `Unknown`).
    fn from(v: ExactVerdict) -> Self {
        match v {
            ExactVerdict::Holds => PropertyVerdict::Holds,
            ExactVerdict::Violated => PropertyVerdict::Violated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_refinement_default_is_empty_and_serde_round_trips() {
        // Common path: an empty refinement serializes to `{}` (every field is skip-if-empty) so it is
        // a zero-cost optional detail on the surfaces.
        let empty = VerdictRefinement::default();
        assert!(empty.is_empty());
        assert_eq!(serde_json::to_string(&empty).unwrap(), "{}");

        // A populated refinement (vacuity + a config partition + an assumption) round-trips.
        let full = VerdictRefinement {
            vacuous: Some(VacuityWitness {
                good_unreachable: true,
                note: "never reached".into(),
            }),
            config_partition: Some(ConfigPartition {
                config_atoms: vec![("cfg".into(), 4)],
                holds: vec![vec![("cfg".into(), 15)]],
                violated: vec![vec![("cfg".into(), 0)]],
                unknown: vec![],
                vacuous: vec![],
                exhaustive: true,
                engine: "exact-symbolic per-config pin".into(),
            }),
            holds_under: vec![DiscoveredAssumption {
                phi: "rst eventually".into(),
                kind: AssumptionKind::ResetEventually,
                non_vacuous: true,
                engine: "native-bmc".into(),
                notes: Vec::new(),
            }],
            bot_diagnosis: None,
        };
        assert!(!full.is_empty());
        let json = serde_json::to_string(&full).unwrap();
        let back: VerdictRefinement = serde_json::from_str(&json).unwrap();
        assert_eq!(
            full, back,
            "VerdictRefinement must survive a JSON round-trip"
        );
    }

    #[test]
    fn labels_are_the_canonical_vocabulary() {
        assert_eq!(PropertyVerdict::Holds.as_str(), "holds");
        assert_eq!(PropertyVerdict::Violated.as_str(), "violated");
        assert_eq!(PropertyVerdict::Unknown.as_str(), "unknown");
        assert_eq!(PropertyVerdict::Skipped.as_str(), "skipped");
    }

    #[test]
    fn reach_verdict_maps_to_the_safety_reading() {
        assert_eq!(
            PropertyVerdict::from(ReachVerdict::Unreachable),
            PropertyVerdict::Holds
        );
        assert_eq!(
            PropertyVerdict::from(ReachVerdict::Reachable),
            PropertyVerdict::Violated
        );
        assert_eq!(
            PropertyVerdict::from(ReachVerdict::Unknown),
            PropertyVerdict::Unknown
        );
        // A contradiction is undecided at the property level; the alarm is a flag.
        assert_eq!(
            PropertyVerdict::from(ReachVerdict::Contradiction),
            PropertyVerdict::Unknown
        );
    }

    #[test]
    fn liveness_inconclusive_folds_to_unknown() {
        assert_eq!(
            PropertyVerdict::from(LivenessVerdict::Inconclusive),
            PropertyVerdict::Unknown
        );
        assert_eq!(
            PropertyVerdict::from(LivenessVerdict::Holds),
            PropertyVerdict::Holds
        );
    }
}
