//! LTL (Linear Temporal Logic) AST representation.
//!
//! This module defines the abstract syntax tree for LTL formulas, including
//! all temporal operators (G, F, X, U, W, R) and propositional operators.

/// LTL formula represented as an abstract syntax tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LtlFormula {
    // Atomic formulas
    /// Truth constant: `true`
    True,
    /// Falsity constant: `false`
    False,
    /// State predicate: `p`
    Predicate(String),

    // Propositional operators
    /// Negation: `!φ`
    Not(Box<LtlFormula>),
    /// Conjunction: `φ && ψ`
    And(Box<LtlFormula>, Box<LtlFormula>),
    /// Disjunction: `φ || ψ`
    Or(Box<LtlFormula>, Box<LtlFormula>),
    /// Implication: `φ -> ψ`
    Implies(Box<LtlFormula>, Box<LtlFormula>),

    // Temporal operators (basic)
    /// Next: `X φ` - In the next step, φ holds
    Next(Box<LtlFormula>),
    /// Always: `G φ` - Globally, φ always holds
    Always(Box<LtlFormula>),
    /// Eventually: `F φ` - Eventually φ (finally)
    Eventually(Box<LtlFormula>),
    /// Until: `φ U ψ` - φ holds until ψ happens (and ψ eventually happens)
    Until {
        left: Box<LtlFormula>,
        right: Box<LtlFormula>,
    },
    /// Weak until: `φ W ψ` - φ holds until ψ happens, or φ always holds
    WeakUntil {
        left: Box<LtlFormula>,
        right: Box<LtlFormula>,
    },
    /// Release: `φ R ψ` - ψ holds until φ releases it
    Release {
        left: Box<LtlFormula>,
        right: Box<LtlFormula>,
    },

    // Derived patterns (for convenience)
    /// Recurrence: `GF φ` - Infinitely often φ (always eventually)
    Recurrence(Box<LtlFormula>),
    /// Stabilization: `FG φ` - Eventually forever φ (stabilization)
    Stabilization(Box<LtlFormula>),
    /// Response: `G(φ -> F(ψ))` - Every request is eventually granted (responsiveness)
    Response {
        trigger: Box<LtlFormula>,
        response: Box<LtlFormula>,
    },
}

/// Prints the formula in the usual LTL syntax (`G`, `F`, `X`, `U`, `W`, `R`, `->`, `&&`,
/// `||`, `!`, atoms), so a message can quote a clause in the user's own words (mununu#1121).
/// Every compound operand is parenthesised; the text reads back under any precedence.
impl std::fmt::Display for LtlFormula {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LtlFormula::True => f.write_str("true"),
            LtlFormula::False => f.write_str("false"),
            LtlFormula::Predicate(p) => f.write_str(p),
            LtlFormula::Not(x) => write!(f, "!{}", Operand(x)),
            LtlFormula::And(a, b) => write!(f, "{} && {}", Operand(a), Operand(b)),
            LtlFormula::Or(a, b) => write!(f, "{} || {}", Operand(a), Operand(b)),
            LtlFormula::Implies(a, b) => write!(f, "{} -> {}", Operand(a), Operand(b)),
            LtlFormula::Next(x) => write!(f, "X {}", Operand(x)),
            LtlFormula::Always(x) => write!(f, "G {}", Operand(x)),
            LtlFormula::Eventually(x) => write!(f, "F {}", Operand(x)),
            LtlFormula::Until { left, right } => {
                write!(f, "{} U {}", Operand(left), Operand(right))
            }
            LtlFormula::WeakUntil { left, right } => {
                write!(f, "{} W {}", Operand(left), Operand(right))
            }
            LtlFormula::Release { left, right } => {
                write!(f, "{} R {}", Operand(left), Operand(right))
            }
            LtlFormula::Recurrence(x) => write!(f, "G F {}", Operand(x)),
            LtlFormula::Stabilization(x) => write!(f, "F G {}", Operand(x)),
            LtlFormula::Response { trigger, response } => {
                write!(f, "G ({} -> F {})", Operand(trigger), Operand(response))
            }
        }
    }
}

/// An operand of a compound operator: an atom prints bare, anything else in parentheses.
struct Operand<'a>(&'a LtlFormula);

impl std::fmt::Display for Operand<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            LtlFormula::True | LtlFormula::False | LtlFormula::Predicate(_) => {
                write!(f, "{}", self.0)
            }
            other => write!(f, "({other})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LtlFormula;

    #[test]
    fn test_ltl_ast_creation() {
        // Test atomic formulas
        let true_formula = LtlFormula::True;
        let false_formula = LtlFormula::False;
        let predicate = LtlFormula::Predicate("safe".to_string());

        assert!(matches!(true_formula, LtlFormula::True));
        assert!(matches!(false_formula, LtlFormula::False));
        assert!(matches!(predicate, LtlFormula::Predicate(ref s) if s == "safe"));

        // Test propositional operators
        let not_formula = LtlFormula::Not(Box::new(LtlFormula::Predicate("deadlock".to_string())));
        let and_formula = LtlFormula::And(
            Box::new(LtlFormula::Predicate("safe".to_string())),
            Box::new(LtlFormula::Predicate("bounded".to_string())),
        );
        let or_formula = LtlFormula::Or(
            Box::new(LtlFormula::Predicate("error".to_string())),
            Box::new(LtlFormula::Predicate("warning".to_string())),
        );
        let implies_formula = LtlFormula::Implies(
            Box::new(LtlFormula::Predicate("request".to_string())),
            Box::new(LtlFormula::Predicate("grant".to_string())),
        );

        assert!(matches!(not_formula, LtlFormula::Not(_)));
        assert!(matches!(and_formula, LtlFormula::And(_, _)));
        assert!(matches!(or_formula, LtlFormula::Or(_, _)));
        assert!(matches!(implies_formula, LtlFormula::Implies(_, _)));

        // Test temporal operators
        let next_formula = LtlFormula::Next(Box::new(LtlFormula::Predicate("alarm".to_string())));
        let always_formula =
            LtlFormula::Always(Box::new(LtlFormula::Predicate("safe".to_string())));
        let eventually_formula =
            LtlFormula::Eventually(Box::new(LtlFormula::Predicate("completed".to_string())));
        let until_formula = LtlFormula::Until {
            left: Box::new(LtlFormula::Predicate("request".to_string())),
            right: Box::new(LtlFormula::Predicate("grant".to_string())),
        };
        let weak_until_formula = LtlFormula::WeakUntil {
            left: Box::new(LtlFormula::Predicate("request".to_string())),
            right: Box::new(LtlFormula::Predicate("grant".to_string())),
        };
        let release_formula = LtlFormula::Release {
            left: Box::new(LtlFormula::Predicate("request".to_string())),
            right: Box::new(LtlFormula::Predicate("grant".to_string())),
        };

        assert!(matches!(next_formula, LtlFormula::Next(_)));
        assert!(matches!(always_formula, LtlFormula::Always(_)));
        assert!(matches!(eventually_formula, LtlFormula::Eventually(_)));
        assert!(matches!(until_formula, LtlFormula::Until { .. }));
        assert!(matches!(weak_until_formula, LtlFormula::WeakUntil { .. }));
        assert!(matches!(release_formula, LtlFormula::Release { .. }));

        // Test derived patterns
        let recurrence_formula =
            LtlFormula::Recurrence(Box::new(LtlFormula::Predicate("heartbeat".to_string())));
        let stabilization_formula =
            LtlFormula::Stabilization(Box::new(LtlFormula::Predicate("idle".to_string())));
        let response_formula = LtlFormula::Response {
            trigger: Box::new(LtlFormula::Predicate("request".to_string())),
            response: Box::new(LtlFormula::Predicate("grant".to_string())),
        };

        assert!(matches!(recurrence_formula, LtlFormula::Recurrence(_)));
        assert!(matches!(
            stabilization_formula,
            LtlFormula::Stabilization(_)
        ));
        assert!(matches!(response_formula, LtlFormula::Response { .. }));
    }

    #[test]
    fn test_ltl_ast_debug() {
        let formula = LtlFormula::Always(Box::new(LtlFormula::Predicate("safe".to_string())));
        let debug_str = format!("{:?}", formula);
        assert!(debug_str.contains("Always"));
        assert!(debug_str.contains("Predicate"));
        assert!(debug_str.contains("safe"));
    }

    #[test]
    fn test_ltl_ast_clone() {
        let formula = LtlFormula::And(
            Box::new(LtlFormula::Always(Box::new(LtlFormula::Predicate(
                "safe".to_string(),
            )))),
            Box::new(LtlFormula::Eventually(Box::new(LtlFormula::Predicate(
                "completed".to_string(),
            )))),
        );
        let cloned = formula.clone();
        assert_eq!(formula, cloned);
    }

    #[test]
    fn test_ltl_ast_equality() {
        let formula1 = LtlFormula::Predicate("safe".to_string());
        let formula2 = LtlFormula::Predicate("safe".to_string());
        let formula3 = LtlFormula::Predicate("unsafe".to_string());

        assert_eq!(formula1, formula2);
        assert_ne!(formula1, formula3);
    }

    /// mununu#1121 — a formula prints in the usual LTL syntax, with compound operands
    /// parenthesised, never as the Rust `Debug` tree.
    #[test]
    fn x1121_an_ltl_formula_displays_in_the_usual_syntax() {
        let p = |s: &str| Box::new(LtlFormula::Predicate(s.to_string()));
        let next_not_grant = LtlFormula::Always(Box::new(LtlFormula::Implies(
            p("grant"),
            Box::new(LtlFormula::Next(Box::new(LtlFormula::Not(p("grant"))))),
        )));
        assert_eq!(next_not_grant.to_string(), "G (grant -> (X (!grant)))");
        let response = LtlFormula::Response {
            trigger: p("req"),
            response: p("grant"),
        };
        assert_eq!(response.to_string(), "G (req -> F grant)");
        assert_eq!(LtlFormula::Recurrence(p("req")).to_string(), "G F req");
        let until = LtlFormula::Until {
            left: Box::new(LtlFormula::And(p("a"), p("b"))),
            right: Box::new(LtlFormula::True),
        };
        assert_eq!(until.to_string(), "(a && b) U true");
    }
}
