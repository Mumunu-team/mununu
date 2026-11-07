//! Shared controllability classifier — task A4 of
//! `docs/archive/design/black-box-modules.md`.
//!
//! The rule, restated from Document A §4:
//!
//! > The controllability of a transition label is the controllability of
//! > whichever side of the boundary drives it.
//!
//! In practice this collapses to: classify labels by the direction of the
//! driving port at the *current scope's boundary*. Inputs (driven by the
//! environment) become `Uncontrollable`; outputs (driven by the module
//! the surrounding logic owns) become `Controllable`; signals that do
//! not cross the boundary are `Internal`.
//!
//! Today's pipelines each compute the classification differently because
//! they receive different inputs:
//!
//! - **Custom SV** has port directions per module — uses this helper
//!   directly. The same `BoundaryDirection` enum applies.
//! - **BTOR2** loses port-direction information through yosys's
//!   `flatten`; falls back to "all uncontrollable" plus a CLI override
//!   list. The unification recommendation (Document B §B.3) is to preserve
//!   the original port directions and feed them through this helper.
//!   Until that lands, the BTOR2 path keeps the CLI list as an escape
//!   hatch.
//! - **Software extraction** has no native notion of port direction;
//!   domain profiles approximate it with method-name globs. Those globs
//!   are a domain-specific approximation of this rule — they should be
//!   read as "is this method called from outside or inside the
//!   component's boundary?"
//!
//! The shared helper in this module is the canonical answer; the three
//! adapter callers funnel through it when they have direction info.

use crate::clts::LabelControllability;

/// Direction of a signal / port / call relative to the surrounding
/// scope's boundary. Inputs are driven from outside the boundary;
/// outputs are driven from inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum BoundaryDirection {
    /// Signal/port/call driven by the environment outside the boundary.
    Input,
    /// Signal/port/call driven by the module inside the boundary.
    Output,
    /// Bidirectional / `inout` style. Treated as neither side has full
    /// control; mapped to `Internal` by default (callers can override
    /// when the semantics demand it).
    Inout,
    /// Does not cross the boundary — hidden from the rest of the design.
    Internal,
}

/// The canonical mapping from boundary direction to label
/// controllability. **This is the unification target referenced by
/// Document A §4 / Document B §B.3 — all three pipelines should
/// eventually funnel through it.**
///
/// - `Input` → `Uncontrollable` (environment drives).
/// - `Output` → `Controllable` (surrounding logic drives).
/// - `Inout` → `Internal` (no single owner; can be promoted by an
///   explicit override).
/// - `Internal` → `Internal`.
pub fn classify_from_direction(direction: BoundaryDirection) -> LabelControllability {
    match direction {
        BoundaryDirection::Input => LabelControllability::Uncontrollable,
        BoundaryDirection::Output => LabelControllability::Controllable,
        BoundaryDirection::Inout | BoundaryDirection::Internal => LabelControllability::Internal,
    }
}

/// Classify a label with optional per-name overrides for unusual cases
/// (a designer who wants to treat a normally-output signal as adversarial
/// for a particular property, etc.). Overrides win over direction.
///
/// `force_controllable` and `force_uncontrollable` are the lists the
/// adapter received via CLI / annotation. They are *escape hatches* — not
/// the primary mechanism (per Document A §4.ii).
pub fn classify_label(
    name: &str,
    direction: BoundaryDirection,
    force_controllable: &[&str],
    force_uncontrollable: &[&str],
) -> LabelControllability {
    if force_controllable.contains(&name) {
        return LabelControllability::Controllable;
    }
    if force_uncontrollable.contains(&name) {
        return LabelControllability::Uncontrollable;
    }
    classify_from_direction(direction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_maps_to_uncontrollable() {
        assert_eq!(
            classify_from_direction(BoundaryDirection::Input),
            LabelControllability::Uncontrollable
        );
    }

    #[test]
    fn output_maps_to_controllable() {
        assert_eq!(
            classify_from_direction(BoundaryDirection::Output),
            LabelControllability::Controllable
        );
    }

    #[test]
    fn inout_maps_to_internal_by_default() {
        assert_eq!(
            classify_from_direction(BoundaryDirection::Inout),
            LabelControllability::Internal
        );
    }

    #[test]
    fn internal_stays_internal() {
        assert_eq!(
            classify_from_direction(BoundaryDirection::Internal),
            LabelControllability::Internal
        );
    }

    #[test]
    fn override_wins_over_direction() {
        // An output that the designer wants to treat as adversarial.
        let result = classify_label("ack", BoundaryDirection::Output, &[], &["ack"]);
        assert_eq!(result, LabelControllability::Uncontrollable);
    }

    #[test]
    fn explicit_controllable_override_promotes_input() {
        // An input the designer wants to drive deterministically for the
        // property at hand (e.g., a reset).
        let result = classify_label("reset_n", BoundaryDirection::Input, &["reset_n"], &[]);
        assert_eq!(result, LabelControllability::Controllable);
    }

    #[test]
    fn controllable_override_beats_uncontrollable_override() {
        // Both lists name the same label — the controllable list wins
        // because it is checked first. Document this so future readers
        // know the precedence.
        let result = classify_label("weird", BoundaryDirection::Input, &["weird"], &["weird"]);
        assert_eq!(result, LabelControllability::Controllable);
    }
}
