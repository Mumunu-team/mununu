//! LTL to μ-Calculus translator.
//!
//! This module translates LTL formulas into μ-calculus formulas following the
//! patterns defined in `docs/ltl_templates/ai_ltl_to_mu_cheatsheet.json`.

use thiserror::Error;

use crate::mu_calculus::{Formula, FormulaBuilder, FormulaVarId, Guard, ModalKind, Node, NodeId};

use super::ast::LtlFormula;

/// Threshold above which `translate` emits an alternation-depth warning. The
/// μ-calculus model-checking complexity is O(|S|^k) where k is the alternation
/// depth (Emerson-Jutla bound), so depth ≥ 4 begins to produce noticeable
/// performance degradation on non-trivial state spaces.
const ALTERNATION_DEPTH_WARN_THRESHOLD: usize = 3;

/// Translates an LTL formula to a μ-calculus formula.
///
/// Emits a `[mununu] WARN` line on stderr when the produced formula has
/// alternation depth ≥ 4 — verification will be slow because the fixpoint
/// engine's worst-case complexity is `O(|S|^k)` (Emerson-Jutla 1991). priority_roadmap §3.4 / Tier A4.
pub fn translate(ltl: &LtlFormula) -> Result<Formula, TranslationError> {
    let mut translator = Translator::new();
    let root = translator.translate_formula(ltl)?;
    let formula = translator.builder.into_formula(root);
    let depth = formula.alternation_depth();
    if depth > ALTERNATION_DEPTH_WARN_THRESHOLD {
        eprintln!(
            "[mununu] WARN: LTL→μ-calculus produced a formula with alternation depth {}; \
             verification may be slow (worst-case complexity O(|S|^{})). Consider rewriting \
             the LTL formula in a lower-alternation form if possible.",
            depth, depth
        );
    }
    Ok(formula)
}

/// Errors that can occur during LTL to μ-calculus translation.
#[derive(Debug, Error)]
pub enum TranslationError {
    #[error("translation error: {message}")]
    Translation { message: String },
}

struct Translator {
    builder: FormulaBuilder,
    var_counter: usize,
}

impl Translator {
    fn new() -> Self {
        Self {
            builder: FormulaBuilder::default(),
            var_counter: 0,
        }
    }

    fn translate_formula(&mut self, ltl: &LtlFormula) -> Result<NodeId, TranslationError> {
        match ltl {
            // Atomic formulas
            LtlFormula::True => Ok(self.builder.push_node(Node::True)),
            LtlFormula::False => Ok(self.builder.push_node(Node::False)),
            LtlFormula::Predicate(name) => {
                Ok(self.builder.push_node(Node::Predicate(name.clone())))
            }

            // Propositional operators
            LtlFormula::Not(inner) => {
                let inner_id = self.translate_formula(inner)?;
                Ok(self.builder.push_node(Node::Not(inner_id)))
            }

            LtlFormula::And(left, right) => {
                let left_id = self.translate_formula(left)?;
                let right_id = self.translate_formula(right)?;
                Ok(self.builder.push_node(Node::And(left_id, right_id)))
            }

            LtlFormula::Or(left, right) => {
                let left_id = self.translate_formula(left)?;
                let right_id = self.translate_formula(right)?;
                Ok(self.builder.push_node(Node::Or(left_id, right_id)))
            }

            LtlFormula::Implies(left, right) => {
                // φ -> ψ = !φ || ψ
                let left_id = self.translate_formula(left)?;
                let right_id = self.translate_formula(right)?;
                let not_left = self.builder.push_node(Node::Not(left_id));
                Ok(self.builder.push_node(Node::Or(not_left, right_id)))
            }

            // Temporal operators (basic)
            LtlFormula::Next(inner) => {
                // X φ = [] φ
                let inner_id = self.translate_formula(inner)?;
                Ok(self.box_modal(inner_id))
            }

            LtlFormula::Always(inner) => self.translate_always(inner),

            LtlFormula::Eventually(inner) => self.translate_eventually(inner),

            LtlFormula::Until { left, right } => self.translate_until(left, right),

            LtlFormula::WeakUntil { left, right } => {
                // φ W ψ = (φ U ψ) ∨ G φ = μ X. (ψ ∨ (φ ∧ [] X)) ∨ (ν Y. (φ ∧ [] Y))
                let until_id = self.translate_until(left, right)?;
                let always_left_id = self.translate_always(left)?;
                Ok(self.builder.push_node(Node::Or(until_id, always_left_id)))
            }

            LtlFormula::Release { left, right } => {
                // φ R ψ = !(!φ U !ψ) = !(μ X. (!ψ ∨ (!φ ∧ [] X)))
                let left_id = self.translate_formula(left)?;
                let right_id = self.translate_formula(right)?;
                let not_left = self.builder.push_node(Node::Not(left_id));
                let not_right = self.builder.push_node(Node::Not(right_id));
                let until_id = self.translate_until_internal(not_left, not_right)?;
                Ok(self.builder.push_node(Node::Not(until_id)))
            }

            // Derived patterns
            LtlFormula::Recurrence(inner) => {
                // GF φ = G F φ = ν Y. (μ X. (φ ∨ <> X) ∧ [] Y)
                let eventually_id = self.translate_eventually(inner)?;
                let var_id = self.new_fixpoint_var("Y");
                let var_node = self.builder.push_node(Node::Variable(var_id));
                let box_var = self.box_modal(var_node);
                let and_node = self.builder.push_node(Node::And(eventually_id, box_var));
                Ok(self.builder.push_node(Node::Nu {
                    var: var_id,
                    body: and_node,
                }))
            }

            LtlFormula::Stabilization(inner) => {
                // FG φ = F G φ = μ Y. (ν X. (φ ∧ [] X) ∨ <> Y)
                let always_id = self.translate_always(inner)?;
                let var_id = self.new_fixpoint_var("Y");
                let var_node = self.builder.push_node(Node::Variable(var_id));
                let dia_var = self.diamond_modal(var_node);
                let or_node = self.builder.push_node(Node::Or(always_id, dia_var));
                Ok(self.builder.push_node(Node::Mu {
                    var: var_id,
                    body: or_node,
                }))
            }

            LtlFormula::Response { trigger, response } => {
                // G(φ -> F(ψ)) = ν X. ((!φ ∨ μ Y. (ψ ∨ <> Y)) ∧ [] X)
                let trigger_id = self.translate_formula(trigger)?;
                let response_id = self.translate_formula(response)?;
                let not_trigger = self.builder.push_node(Node::Not(trigger_id));
                let eventually_response = self.translate_eventually_internal(response_id)?;
                let or_node = self
                    .builder
                    .push_node(Node::Or(not_trigger, eventually_response));
                let var_id = self.new_fixpoint_var("X");
                let var_node = self.builder.push_node(Node::Variable(var_id));
                let box_var = self.box_modal(var_node);
                let and_node = self.builder.push_node(Node::And(or_node, box_var));
                Ok(self.builder.push_node(Node::Nu {
                    var: var_id,
                    body: and_node,
                }))
            }
        }
    }

    // Helper methods for internal translations

    /// Creates a boxed modal node `[] target` used throughout the translation.
    ///
    /// Many LTL encodings use the pattern `[] X` (universal next) inside
    /// fixpoint bodies. Centralising this helper keeps the translation
    /// templates concise and consistent.
    fn box_modal(&mut self, target: NodeId) -> NodeId {
        self.builder.push_node(Node::Modal {
            kind: ModalKind::Box,
            guard: Guard::default(),
            target,
        })
    }

    fn diamond_modal(&mut self, target: NodeId) -> NodeId {
        self.builder.push_node(Node::Modal {
            kind: ModalKind::Diamond,
            guard: Guard::default(),
            target,
        })
    }

    fn translate_always(&mut self, inner: &LtlFormula) -> Result<NodeId, TranslationError> {
        // G φ = ν X. (φ ∧ [] X)
        let inner_id = self.translate_formula(inner)?;
        let var_id = self.new_fixpoint_var("X");
        let var_node = self.builder.push_node(Node::Variable(var_id));
        let box_var = self.box_modal(var_node);
        let and_node = self.builder.push_node(Node::And(inner_id, box_var));
        Ok(self.builder.push_node(Node::Nu {
            var: var_id,
            body: and_node,
        }))
    }

    fn translate_eventually(&mut self, inner: &LtlFormula) -> Result<NodeId, TranslationError> {
        // F φ = μ X. (φ ∨ <> X) — the DIAMOND (see `translate_eventually_internal`; the box
        // form `μ X. (φ ∨ [] X)` is inevitability along every path, which `ltl F` is not —
        // mununu#593). The LTL page documents the same.
        let inner_id = self.translate_formula(inner)?;
        self.translate_eventually_internal(inner_id)
    }

    fn translate_eventually_internal(
        &mut self,
        inner_id: NodeId,
    ) -> Result<NodeId, TranslationError> {
        // F φ = μ X. (φ ∨ <> X)
        // Uses diamond (existential successor) — under the Skolem paradigm,
        // "eventually" means the controller can find a path to φ.
        let var_id = self.new_fixpoint_var("X");
        let var_node = self.builder.push_node(Node::Variable(var_id));
        let dia_var = self.diamond_modal(var_node);
        let or_node = self.builder.push_node(Node::Or(inner_id, dia_var));
        Ok(self.builder.push_node(Node::Mu {
            var: var_id,
            body: or_node,
        }))
    }

    fn translate_until(
        &mut self,
        left: &LtlFormula,
        right: &LtlFormula,
    ) -> Result<NodeId, TranslationError> {
        // φ U ψ = μ X. (ψ ∨ (φ ∧ [] X))
        let left_id = self.translate_formula(left)?;
        let right_id = self.translate_formula(right)?;
        self.translate_until_internal(left_id, right_id)
    }

    fn translate_until_internal(
        &mut self,
        left_id: NodeId,
        right_id: NodeId,
    ) -> Result<NodeId, TranslationError> {
        // φ U ψ = μ X. (ψ ∨ (φ ∧ <> X))
        // Uses diamond (existential successor) — the controller must find a
        // path where ψ eventually holds while φ holds along the way.
        let var_id = self.new_fixpoint_var("X");
        let var_node = self.builder.push_node(Node::Variable(var_id));
        let dia_var = self.diamond_modal(var_node);
        let and_left = self.builder.push_node(Node::And(left_id, dia_var));
        let or_node = self.builder.push_node(Node::Or(right_id, and_left));
        Ok(self.builder.push_node(Node::Mu {
            var: var_id,
            body: or_node,
        }))
    }

    fn new_fixpoint_var(&mut self, prefix: &str) -> FormulaVarId {
        let name = format!("{}{}", prefix, self.var_counter);
        self.var_counter += 1;
        self.builder.push_var(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ltl::parser::parse as parse_ltl;
    use crate::mu_calculus::parser::parse as parse_mu;

    fn translate_ltl(ltl_str: &str) -> Formula {
        let ltl = parse_ltl(ltl_str).expect("LTL should parse");
        translate(&ltl).expect("Translation should succeed")
    }

    fn parse_mu_str(mu_str: &str) -> Formula {
        parse_mu(mu_str).expect("μ-calculus should parse")
    }

    #[test]
    fn test_translate_true() {
        let translated = translate_ltl("true");
        let expected = parse_mu_str("true");
        assert!(matches!(translated.node(translated.root()), Node::True));
        assert!(matches!(expected.node(expected.root()), Node::True));
    }

    #[test]
    fn test_translate_false() {
        let translated = translate_ltl("false");
        let expected = parse_mu_str("false");
        assert!(matches!(translated.node(translated.root()), Node::False));
        assert!(matches!(expected.node(expected.root()), Node::False));
    }

    #[test]
    fn test_translate_predicate() {
        let translated = translate_ltl("safe");
        let expected = parse_mu_str("safe");
        match (
            translated.node(translated.root()),
            expected.node(expected.root()),
        ) {
            (Node::Predicate(p1), Node::Predicate(p2)) => assert_eq!(p1, p2),
            _ => panic!("Expected predicates"),
        }
    }

    #[test]
    fn test_translate_not() {
        let translated = translate_ltl("!deadlock");
        let expected = parse_mu_str("!deadlock");
        match (
            translated.node(translated.root()),
            expected.node(expected.root()),
        ) {
            (Node::Not(_), Node::Not(_)) => {}
            _ => panic!("Expected Not nodes"),
        }
    }

    #[test]
    fn test_translate_and() {
        let translated = translate_ltl("safe && bounded");
        let expected = parse_mu_str("safe && bounded");
        match (
            translated.node(translated.root()),
            expected.node(expected.root()),
        ) {
            (Node::And(_, _), Node::And(_, _)) => {}
            _ => panic!("Expected And nodes"),
        }
    }

    #[test]
    fn test_translate_or() {
        let translated = translate_ltl("error || warning");
        let expected = parse_mu_str("error || warning");
        match (
            translated.node(translated.root()),
            expected.node(expected.root()),
        ) {
            (Node::Or(_, _), Node::Or(_, _)) => {}
            _ => panic!("Expected Or nodes"),
        }
    }

    #[test]
    fn test_translate_implies() {
        let translated = translate_ltl("request -> grant");
        // φ -> ψ = !φ || ψ
        let expected = parse_mu_str("!request || grant");
        match (
            translated.node(translated.root()),
            expected.node(expected.root()),
        ) {
            (Node::Or(_, _), Node::Or(_, _)) => {}
            _ => panic!("Expected Or nodes (implies becomes or)"),
        }
    }

    #[test]
    fn test_translate_next() {
        let translated = translate_ltl("X alarm");
        // X φ = [] φ
        let expected = parse_mu_str("[] alarm");
        match (
            translated.node(translated.root()),
            expected.node(expected.root()),
        ) {
            (
                Node::Modal {
                    kind: ModalKind::Box,
                    ..
                },
                Node::Modal {
                    kind: ModalKind::Box,
                    ..
                },
            ) => {}
            _ => panic!("Expected Box modal nodes"),
        }
    }

    #[test]
    fn test_translate_always() {
        let translated = translate_ltl("G safe");
        // G φ = ν X. (φ ∧ [] X)
        let expected = parse_mu_str("nu X. (safe && [] X)");
        match (
            translated.node(translated.root()),
            expected.node(expected.root()),
        ) {
            (Node::Nu { .. }, Node::Nu { .. }) => {}
            _ => panic!("Expected Nu nodes"),
        }
    }

    #[test]
    fn test_translate_eventually() {
        let translated = translate_ltl("F completed");
        // F φ = μ X. (φ ∨ <> X)
        let expected = parse_mu_str("mu X. (completed || <> X)");
        match (
            translated.node(translated.root()),
            expected.node(expected.root()),
        ) {
            (Node::Mu { .. }, Node::Mu { .. }) => {}
            _ => panic!("Expected Mu nodes"),
        }
    }

    #[test]
    fn test_translate_until() {
        let translated = translate_ltl("request U grant");
        // φ U ψ = μ X. (ψ ∨ (φ ∧ <> X))
        let expected = parse_mu_str("mu X. (grant || (request && <> X))");
        match (
            translated.node(translated.root()),
            expected.node(expected.root()),
        ) {
            (Node::Mu { .. }, Node::Mu { .. }) => {}
            _ => panic!("Expected Mu nodes"),
        }
    }

    #[test]
    fn test_translate_weak_until() {
        let translated = translate_ltl("request W grant");
        // φ W ψ = (φ U ψ) ∨ G φ
        // This should have Or at the top level
        match translated.node(translated.root()) {
            Node::Or(_, _) => {}
            _ => panic!("Expected Or at top level for weak until"),
        }
    }

    #[test]
    fn test_translate_release() {
        let translated = translate_ltl("request R grant");
        // φ R ψ = !(!φ U !ψ)
        // This should have Not at the top level
        match translated.node(translated.root()) {
            Node::Not(_) => {}
            _ => panic!("Expected Not at top level for release"),
        }
    }

    #[test]
    fn test_translate_recurrence() {
        let translated = translate_ltl("G F heartbeat");
        // GF φ = ν Y. (μ X. (φ ∨ [] X) ∧ [] Y)
        match translated.node(translated.root()) {
            Node::Nu { .. } => {}
            _ => panic!("Expected Nu at top level for recurrence"),
        }
    }

    #[test]
    fn test_translate_stabilization() {
        let translated = translate_ltl("F G idle");
        // FG φ = μ Y. (ν X. (φ ∧ [] X) ∨ [] Y)
        match translated.node(translated.root()) {
            Node::Mu { .. } => {}
            _ => panic!("Expected Mu at top level for stabilization"),
        }
    }

    #[test]
    fn test_translate_response() {
        let translated = translate_ltl("G (request -> F grant)");
        // G(φ -> F(ψ)) = ν X. ((!φ ∨ μ Y. (ψ ∨ [] Y)) ∧ [] X)
        match translated.node(translated.root()) {
            Node::Nu { .. } => {}
            _ => panic!("Expected Nu at top level for response"),
        }
    }

    #[test]
    fn test_translate_nested() {
        let translated = translate_ltl("G (!deadlock && (request -> F grant))");
        match translated.node(translated.root()) {
            Node::Nu { .. } => {}
            _ => panic!("Expected Nu at top level for nested formula"),
        }
    }

    #[test]
    fn test_translate_fixpoint_names() {
        // Test that fixpoint variables get unique names
        let translated = translate_ltl("G F heartbeat");
        // Should have two fixpoint variables (one for G, one for F)
        assert!(translated.vars().len() >= 2);

        // Variable names should be unique
        let var_names: Vec<&str> = translated.vars().iter().map(|v| v.name.as_str()).collect();
        let unique_names: std::collections::HashSet<&str> = var_names.iter().cloned().collect();
        assert_eq!(
            var_names.len(),
            unique_names.len(),
            "Fixpoint variable names should be unique"
        );
    }
}
