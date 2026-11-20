//! Controllability validation helpers for CLTS composition.
//!
//! This module centralises the logic that:
//! - validates mutual exclusivity of controllable/internal actions across CLTSs, and
//! - inspects composed transitions for uncontrollable labels.

use std::collections::HashSet;

use crate::clts::{
    Clts, CltsError, CltsResult, DefaultLabelIdx, DefaultStateIdx, LabelId, Transition,
};

/// Helper for validating controllability constraints during composition.
#[derive(Debug, Default)]
pub(crate) struct ControllabilityChecker;

impl ControllabilityChecker {
    /// Validates that controllable and internal actions are mutually exclusive between automata.
    ///
    /// Rule: When composing CLTSs, controllable and internal actions must be mutually exclusive.
    /// No two automata taking part in a composition should share controllable or internal actions.
    /// Uncontrollable actions (e.g., input signals) can be shared, as they are controlled by the
    /// environment.
    pub(crate) fn validate_composition(
        left: &Clts<DefaultStateIdx, DefaultLabelIdx>,
        right: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    ) -> CltsResult<()> {
        // Collect controllable label names from both CLTSs.
        let left_controllable_names = Self::collect_label_names(left, left.controllable_alphabet());
        let right_controllable_names =
            Self::collect_label_names(right, right.controllable_alphabet());

        // Check for shared controllable actions (by name).
        let shared_controllable: Vec<String> = left_controllable_names
            .intersection(&right_controllable_names)
            .cloned()
            .collect();

        if !shared_controllable.is_empty() {
            return Err(CltsError::CompositionError(format!(
                "controllable actions shared between automata: {}",
                shared_controllable.join(", ")
            )));
        }

        // Collect internal label names from both CLTSs.
        let left_internal_names = Self::collect_label_names(left, left.internal_alphabet());
        let right_internal_names = Self::collect_label_names(right, right.internal_alphabet());

        // Check for shared internal actions (by name).
        let shared_internal: Vec<String> = left_internal_names
            .intersection(&right_internal_names)
            .cloned()
            .collect();

        if !shared_internal.is_empty() {
            return Err(CltsError::CompositionError(format!(
                "internal actions shared between automata: {}",
                shared_internal.join(", ")
            )));
        }

        Ok(())
    }

    /// Collects the concrete label names for all labels in `alphabet`.
    pub(crate) fn collect_label_names(
        clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
        alphabet: &HashSet<LabelId<DefaultLabelIdx>>,
    ) -> HashSet<String> {
        let mut names = HashSet::new();
        for &label_id in alphabet {
            if let Some(payload) = clts.label_payload(label_id) {
                for name in payload {
                    names.insert(name.clone());
                }
            }
        }
        names
    }

    /// Returns `true` if `transition` has any uncontrollable labels in `clts`.
    #[inline]
    pub(crate) fn has_uncontrollable_labels(
        transition: &Transition<DefaultStateIdx, DefaultLabelIdx>,
        clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    ) -> bool {
        transition.is_uncontrollable(clts)
    }

    /// Determines whether the composed transition formed from `left` and `right`
    /// has uncontrollable labels.
    ///
    /// Rule: if either component transition is uncontrollable, the composed transition
    /// is treated as uncontrollable.
    #[inline]
    pub(crate) fn composed_has_uncontrollable_labels(
        left: &Transition<DefaultStateIdx, DefaultLabelIdx>,
        right: &Transition<DefaultStateIdx, DefaultLabelIdx>,
        left_clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
        right_clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    ) -> bool {
        Self::has_uncontrollable_labels(left, left_clts)
            || Self::has_uncontrollable_labels(right, right_clts)
    }
}
